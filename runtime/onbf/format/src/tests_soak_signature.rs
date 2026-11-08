//! Seeded multi-fault soak of the signature-block parser (ONBF §7.1, §13 item 3; S1–S4).
//!
//! The judge is a model of the block written from `docs/spec/ONBF-signature-encoding.md` and
//! Appendix A, on top of the OCBOR-1 oracle: it decides every block on its own, so the
//! parser must agree in both directions. An accepted block must also re-encode from the
//! parsed view to exactly its own bytes.
//!
//! A failure is reproduced by its seed and case number; `ORIGIN_MUTATION_SCALE` scales the
//! case counts.

use crate::signature::{MAGIC, MAX_ENTRIES, Role, SignatureBlock, SignatureError, Suite};
use crate::testing::{Reached, Rng, cases, mutate};
use crate::tests_soak_ocbor::{V, encoded, oracle, random_value};

// ------------------------------------------------------------------ a block, as data

type Fields = Vec<(u64, V)>;

fn suite_of(role: u64) -> u64 {
    if matches!(role, 4 | 5) { 2 } else { 1 }
}

fn mldsa_len(suite: u64) -> usize {
    if suite == 2 { 4627 } else { 3309 }
}

fn statement(role: u64) -> V {
    V::M(vec![
        (0, V::U(1)),
        (1, V::U(6)),
        (2, V::B(vec![0xAB; 64])),
        (3, V::T("origin.framekernel".to_owned())),
        (4, V::T("0.1.0-dev".to_owned())),
        (5, V::U(role)),
        (6, V::U(1_794_000_000)),
    ])
}

fn certificate(rng: &mut Rng) -> V {
    V::M(vec![
        (0, V::U(1)),
        (1, V::B(vec![rng.byte(); 16])),
        (2, random_value(rng, 1)),
    ])
}

fn entry(rng: &mut Rng, role: u64, key: u8) -> Fields {
    let suite = suite_of(role);
    let chain_len = rng.below(5);
    vec![
        (1, V::U(role)),
        (2, V::U(suite)),
        (3, V::B(vec![key; 32])),
        (4, V::A((0..chain_len).map(|_| certificate(rng)).collect())),
        (5, statement(role)),
        (6, V::B(vec![0x22; 64])),
        (7, V::B(vec![0x33; mldsa_len(suite)])),
    ]
}

fn block_of(bodies: &[Vec<u8>]) -> Vec<u8> {
    let mut b = MAGIC.to_vec();
    b.extend(1u32.to_le_bytes());
    b.extend((bodies.len() as u32).to_le_bytes());
    for body in bodies {
        b.extend((body.len() as u32).to_le_bytes());
        b.extend(body);
    }
    b
}

// ------------------------------------------------------------------ faults

const VERSIONS: [&str; 14] = [
    "0.1.0-dev",
    "1.0.0",
    "1.2.3-alpha.1",
    "1.2.3+build.5",
    "1.2.3-rc.1+exp.sha.5114f85",
    "1",
    "1.2",
    "1.2.3.4",
    "01.2.3",
    "1.2.3-01",
    "1.2.3-",
    "1.2.3+",
    "a.b.c",
    "\u{0}.1.0",
];

fn set(fields: &mut Fields, key: u64, value: V) {
    match fields.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => fields.push((key, value)),
    }
}

/// One fault in an entry's fields. Keys stay sorted (except where disorder is the point) and
/// the sizes are rebuilt, so the checks behind the framing are what refuses the bad ones.
fn entry_fault(e: &mut Fields, rng: &mut Rng) {
    let wrong = random_value(rng, 1);
    let statement_key = rng.pick(&[0, 1, 2, 3, 4, 5, 6]);
    match rng.below(24) {
        // Keep the entry valid under another role, so accepted blocks stay varied.
        0 | 1 => {
            let role = 1 + rng.below(5) as u64;
            let suite = suite_of(role);
            set(e, 1, V::U(role));
            set(e, 2, V::U(suite));
            set(e, 7, V::B(vec![rng.byte(); mldsa_len(suite)]));
            if let Some((_, V::M(st))) = e.iter_mut().find(|(k, _)| *k == 5) {
                set(st, 5, V::U(role));
            }
        }
        2 => set(e, 1, V::U(rng.pick(&[0, 6, 7, u64::MAX]))),
        3 => set(e, 2, V::U(rng.pick(&[0, 1, 2, 3, u64::MAX]))),
        4 => set(e, 3, V::B(vec![rng.byte(); 32])),
        5 => set(e, 3, V::B(vec![0; rng.pick(&[0, 31, 33, 64])])),
        // The chain: every length around the Appendix A limit, and hostile shapes.
        6 | 7 => {
            let chain = (0..rng.below(8)).map(|_| certificate(rng)).collect();
            set(e, 4, V::A(chain));
        }
        8 => set(e, 4, V::A(vec![V::U(1)])),
        9 => set(e, 4, wrong),
        10 => set(e, 6, V::B(vec![0; rng.pick(&[0, 63, 65])])),
        11 => set(e, 7, V::B(vec![0; rng.pick(&[3309, 4627, 0, 1])])),
        12 => set(e, 5, wrong),
        13 => set(e, 8, V::B(Vec::new())),
        14 if !e.is_empty() => {
            let i = rng.below(e.len());
            e.remove(i);
        }
        15 if e.len() >= 2 => {
            let (a, b) = (rng.below(e.len()), rng.below(e.len()));
            e.swap(a, b);
            return;
        }
        // Inside the statement.
        16 | 17 => {
            if let Some((_, V::M(st))) = e.iter_mut().find(|(k, _)| *k == 5) {
                let version = rng.pick(&VERSIONS);
                set(st, 4, V::T(version.to_owned()));
            }
        }
        18 => {
            if let Some((_, V::M(st))) = e.iter_mut().find(|(k, _)| *k == 5) {
                let value = match statement_key {
                    0 => V::U(rng.pick(&[0, 2, u64::MAX])),
                    2 => V::B(vec![0; rng.pick(&[32, 63, 65])]),
                    3 => V::T(String::new()),
                    4 => V::U(1),
                    5 => V::U(rng.pick(&[0, 6, 1, 4, 5])),
                    _ => V::T("x".to_owned()),
                };
                set(st, statement_key, value);
            }
        }
        19 => {
            if let Some((_, V::M(st))) = e.iter_mut().find(|(k, _)| *k == 5) {
                st.retain(|(k, _)| *k != statement_key);
            }
        }
        20 => {
            if let Some((_, V::M(st))) = e.iter_mut().find(|(k, _)| *k == 5) {
                set(st, 7, V::U(0));
            }
        }
        21 => {
            if let Some((_, V::M(st))) = e.iter_mut().find(|(k, _)| *k == 5) {
                set(st, 3, V::T(format!("origin.{}", rng.below(1000))));
                set(st, 1, V::U(rng.edge_u64()));
                set(st, 6, V::U(rng.edge_u64()));
            }
        }
        _ => {}
    }
    e.sort_by_key(|(k, _)| *k);
}

// ------------------------------------------------------------------ the model

/// An entry as S1–S4 and Appendix A define it.
#[derive(Debug, PartialEq, Eq)]
struct ModelEntry {
    role: u64,
    suite: u64,
    key_id: Vec<u8>,
    chain: Vec<u8>,
    chain_len: usize,
    statement_bytes: Vec<u8>,
    profile: u64,
    digest: Vec<u8>,
    id: String,
    version: String,
    time: u64,
    sig_ed: Vec<u8>,
    sig_ml: Vec<u8>,
}

/// A statement's `id`: non-empty printable ASCII (errata O1).
fn id_model(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

/// SemVer 2.0.0 (the grammar at semver.org), which S3 requires of the statement's version.
fn semver_model(version: &str) -> bool {
    fn number(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
    }
    fn identifier(s: &str) -> bool {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
    let (main, build) = match version.split_once('+') {
        Some((m, b)) => (m, Some(b)),
        None => (version, None),
    };
    let (core, pre) = match main.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (main, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| number(p))
        && pre.is_none_or(|p| {
            p.split('.')
                .all(|id| identifier(id) && (!id.bytes().all(|b| b.is_ascii_digit()) || number(id)))
        })
        && build.is_none_or(|b| b.split('.').all(identifier))
}

fn model_entry(tree: &V) -> Option<ModelEntry> {
    let V::M(fields) = tree else { return None };
    let keys: Vec<u64> = fields.iter().map(|(k, _)| *k).collect();
    if keys != [1, 2, 3, 4, 5, 6, 7] {
        return None;
    }
    let (V::U(role), V::U(suite)) = (&fields[0].1, &fields[1].1) else {
        return None;
    };
    if !(1..=5).contains(role) || *suite != suite_of(*role) {
        return None;
    }
    let V::B(key_id) = &fields[2].1 else {
        return None;
    };
    let V::A(chain) = &fields[3].1 else {
        return None;
    };
    // Appendix A: at most four certificates; each is a map (the chain verifier reads them).
    if key_id.len() != 32 || chain.len() > 4 || !chain.iter().all(|c| matches!(c, V::M(_))) {
        return None;
    }
    let V::M(st) = &fields[4].1 else { return None };
    let st_keys: Vec<u64> = st.iter().map(|(k, _)| *k).collect();
    if st_keys != [0, 1, 2, 3, 4, 5, 6] || st[0].1 != V::U(1) || st[5].1 != V::U(*role) {
        return None;
    }
    let (V::U(profile), V::B(digest), V::T(id), V::T(version), V::U(time)) =
        (&st[1].1, &st[2].1, &st[3].1, &st[4].1, &st[6].1)
    else {
        return None;
    };
    if digest.len() != 64 || !id_model(id) || !semver_model(version) {
        return None;
    }
    let (V::B(sig_ed), V::B(sig_ml)) = (&fields[5].1, &fields[6].1) else {
        return None;
    };
    if sig_ed.len() != 64 || sig_ml.len() != mldsa_len(*suite) {
        return None;
    }
    Some(ModelEntry {
        role: *role,
        suite: *suite,
        key_id: key_id.clone(),
        chain: encoded(&fields[3].1),
        chain_len: chain.len(),
        statement_bytes: encoded(&fields[4].1),
        profile: *profile,
        digest: digest.clone(),
        id: id.clone(),
        version: version.clone(),
        time: *time,
        sig_ed: sig_ed.clone(),
        sig_ml: sig_ml.clone(),
    })
}

/// The whole block as §7.1 and Appendix A define it, or `None` if it is invalid.
fn model_block(b: &[u8]) -> Option<Vec<ModelEntry>> {
    if b.len() > 1 << 20 || b.get(..8)? != MAGIC {
        return None;
    }
    let word = |at: usize| Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?));
    if word(8)? != 1 {
        return None;
    }
    let n = usize::try_from(word(12)?).ok()?;
    if n == 0 || n > MAX_ENTRIES {
        return None;
    }
    let mut pos = 16;
    let mut entries: Vec<ModelEntry> = Vec::new();
    for _ in 0..n {
        let size = usize::try_from(word(pos)?).ok()?;
        pos += 4;
        let end = pos.checked_add(size)?;
        let body = b.get(pos..end)?;
        let model = model_entry(&oracle(body).ok()?)?;
        if entries
            .iter()
            .any(|e| e.role == model.role && e.key_id == model.key_id)
        {
            return None;
        }
        entries.push(model);
        pos = end;
    }
    (pos == b.len()).then_some(entries)
}

fn error_name(e: SignatureError) -> &'static str {
    match e {
        SignatureError::TooLarge => "TooLarge",
        SignatureError::Magic => "Magic",
        SignatureError::Version(_) => "Version",
        SignatureError::EntryCount(_) => "EntryCount",
        SignatureError::Truncated => "Truncated",
        SignatureError::TrailingData => "TrailingData",
        SignatureError::Ocbor { .. } => "Ocbor",
        SignatureError::Shape { .. } => "Shape",
        SignatureError::Field { .. } => "Field",
        SignatureError::RoleSuite { .. } => "RoleSuite",
        SignatureError::RoleMismatch { .. } => "RoleMismatch",
        SignatureError::Duplicate { .. } => "Duplicate",
    }
}

const REFUSALS: [&str; 13] = [
    "TooLarge",
    "Magic",
    "Version",
    "EntryCount",
    "Truncated",
    "TrailingData",
    "Ocbor",
    "Shape",
    "Field",
    "RoleSuite",
    "RoleMismatch",
    "Duplicate",
    "accepted",
];

/// The block, written back from the parsed view: everything a signature does not cover is
/// taken from what the parser reports, so a misplaced field fails the comparison.
fn reencode(block: &SignatureBlock<'_>) -> Vec<u8> {
    let mut bodies = Vec::new();
    for e in block.entries() {
        let role = e.role as u64;
        let suite = e.suite as u64;
        let mut body = encoded(&V::M(vec![
            (1, V::U(role)),
            (2, V::U(suite)),
            (3, V::B(e.key_id.to_vec())),
        ]));
        // The map head says three entries: rewrite it as seven, then append the raw fields.
        assert_eq!(body[0], 0xA3);
        body[0] = 0xA7;
        body.push(0x04);
        body.extend(e.chain);
        body.push(0x05);
        body.extend(e.statement.bytes);
        body.push(0x06);
        body.extend(encoded(&V::B(e.sig_ed25519.to_vec())));
        body.push(0x07);
        body.extend(encoded(&V::B(e.sig_mldsa.to_vec())));
        bodies.push(body);
    }
    block_of(&bodies)
}

// ------------------------------------------------------------------ the soak

/// `SIGNATURE_SEED`, case `n`, reproduces case `n` of the soak below.
const SIGNATURE_SEED: u64 = 0x4F4E_4246_5349_4731; // "ONBFSIG1"

#[test]
fn mutated_signature_blocks_get_the_models_verdict() {
    let mut reached = Reached::default();
    let total = cases(12_000);
    for case in 0..total {
        let mut rng = Rng::for_case(SIGNATURE_SEED, case);
        let count = match rng.below(16) {
            0 => 8,
            1 => 9,
            2 | 3 => 2 + rng.below(3),
            _ => 1,
        };
        let mut entries: Vec<Fields> = (0..count)
            .map(|i| {
                let role = 1 + rng.below(5) as u64;
                let key = if rng.chance(90) { 0x10 + i as u8 } else { 0x77 };
                entry(&mut rng, role, key)
            })
            .collect();

        let mode = rng.below(10);
        if mode <= 4 {
            for _ in 0..rng.below(4) {
                let i = rng.below(entries.len());
                entry_fault(&mut entries[i], &mut rng);
            }
        }
        let mut bodies: Vec<Vec<u8>> = entries.iter().map(|e| encoded(&V::M(e.clone()))).collect();
        // Raw faults inside one entry, with its size re-sealed, reach the OCBOR-1 checks.
        if (5..=7).contains(&mode) {
            let i = rng.below(bodies.len());
            mutate(&mut bodies[i], &mut rng);
        }
        let mut block = block_of(&bodies);
        match mode {
            8 => mutate(&mut block, &mut rng),
            // The block's own fields, with boundary values, and nothing re-sealed.
            9 => {
                let (at, width) = rng.pick(&[(8, 4), (12, 4), (16, 4)]);
                let value = rng.edge_u64().to_le_bytes();
                block[at..at + width].copy_from_slice(&value[..width]);
            }
            _ => {}
        }
        // Appendix A's 1 MiB, just over and exactly at it, in a form otherwise valid.
        if case % 4096 == 4095 {
            block = oversized_block(&mut rng);
        }

        let model = model_block(&block);
        match SignatureBlock::parse(&block) {
            Err(e) => {
                reached.hit(error_name(e));
                assert!(
                    model.is_none(),
                    "case {case}: refused ({e:?}) a block S1–S4 allow"
                );
            }
            Ok(parsed) => {
                reached.hit("accepted");
                let model = model.unwrap_or_else(|| {
                    panic!(
                        "case {case}: accepted a block S1–S4 and Appendix A refuse: {:02x?}",
                        block.get(..block.len().min(256))
                    )
                });
                assert_eq!(parsed.entries().count(), model.len(), "case {case}");
                assert!(
                    reencode(&parsed) == block,
                    "case {case}: does not re-encode"
                );
                for (e, m) in parsed.entries().zip(&model) {
                    assert_eq!(e.role as u64, m.role, "case {case}");
                    assert_eq!(e.suite as u64, m.suite, "case {case}");
                    assert_eq!(e.suite, e.role.suite(), "case {case}");
                    assert_eq!(e.key_id, m.key_id, "case {case}");
                    assert_eq!(e.chain, m.chain, "case {case}");
                    assert_eq!(e.chain_len, m.chain_len as u64, "case {case}");
                    assert_eq!(e.statement.bytes, m.statement_bytes, "case {case}");
                    assert_eq!(e.statement.profile, m.profile, "case {case}");
                    assert_eq!(e.statement.content_digest, m.digest, "case {case}");
                    assert_eq!(e.statement.id, m.id, "case {case}");
                    assert_eq!(e.statement.version, m.version, "case {case}");
                    assert_eq!(e.statement.role as u64, m.role, "case {case}");
                    assert_eq!(e.statement.claimed_signing_time, m.time, "case {case}");
                    assert_eq!(e.sig_ed25519, m.sig_ed, "case {case}");
                    assert_eq!(e.sig_mldsa, m.sig_ml, "case {case}");
                    assert_eq!(
                        e.sig_mldsa.len(),
                        e.suite.mldsa_signature_len(),
                        "case {case}"
                    );
                }
            }
        }
    }
    reached.require("signature soak", &REFUSALS);
    assert!(
        reached.count("accepted") >= total as u64 / 8,
        "only {} accepted",
        reached.count("accepted")
    );
}

/// A block just over Appendix A's 1 MiB, valid in every other way: two entries, each within
/// the 1 MiB OCBOR-1 document limit, whose certificates together pass the block's limit.
fn oversized_block(rng: &mut Rng) -> Vec<u8> {
    let mut oversize = |key: u8, fill: usize| {
        let mut e = entry(rng, 4, key);
        set(
            &mut e,
            4,
            V::A(vec![V::M(vec![(0, V::B(vec![0x11; fill]))])]),
        );
        encoded(&V::M(e))
    };
    let first = oversize(0x10, 600_000);
    let second = oversize(0x20, 600_000);
    let block = block_of(&[first, second]);
    assert!(block.len() > 1 << 20);
    block
}

#[test]
fn every_role_keeps_its_suite_and_context() {
    for role in 1..=5u64 {
        let parsed = Role::from_raw(role).expect("a defined role");
        assert_eq!(parsed.suite() as u64, suite_of(role));
        assert!(parsed.context().starts_with(b"origin/onbf/1/"));
        let suite = Suite::from_raw(suite_of(role)).unwrap();
        assert_eq!(suite.mldsa_signature_len(), mldsa_len(suite_of(role)));
    }
    assert!(Role::from_raw(0).is_none() && Role::from_raw(6).is_none());
}
