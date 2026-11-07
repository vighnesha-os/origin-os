//! Signature-block parser tests (S1–S4).

use crate::signature::{MAGIC, Role, SignatureBlock, SignatureError, Suite};

/// An OCBOR map under construction: (key, encoded value) pairs.
type Fields = Vec<(u64, Vec<u8>)>;

fn head(major: u8, arg: u64) -> Vec<u8> {
    let m = major << 5;
    match arg {
        0..=23 => vec![m | arg as u8],
        24..=0xFF => vec![m | 24, arg as u8],
        0x100..=0xFFFF => [&[m | 25][..], &(arg as u16).to_be_bytes()].concat(),
        _ => [&[m | 26][..], &(arg as u32).to_be_bytes()].concat(),
    }
}
fn uint(v: u64) -> Vec<u8> {
    head(0, v)
}
fn bytes(b: &[u8]) -> Vec<u8> {
    [head(2, b.len() as u64), b.to_vec()].concat()
}
fn text(t: &str) -> Vec<u8> {
    [head(3, t.len() as u64), t.as_bytes().to_vec()].concat()
}
fn map(entries: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut out = head(5, entries.len() as u64);
    for (k, v) in entries {
        out.extend(uint(*k));
        out.extend_from_slice(v);
    }
    out
}

fn statement(role: u64) -> Fields {
    vec![
        (0, uint(1)),
        (1, uint(6)),
        (2, bytes(&[0xAB; 64])),
        (3, text("origin.framekernel")),
        (4, text("0.1.0-dev")),
        (5, uint(role)),
        (6, uint(1_794_000_000)),
    ]
}

fn entry(role: u64, suite: u64, mldsa_len: usize) -> Fields {
    let cert = map(&[(0, uint(1))]);
    vec![
        (1, uint(role)),
        (2, uint(suite)),
        (3, bytes(&[0x11; 32])),
        (4, [head(4, 1), cert].concat()),
        (5, map(&statement(role))),
        (6, bytes(&[0x22; 64])),
        (7, bytes(&vec![0x33; mldsa_len])),
    ]
}

fn block(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut b = MAGIC.to_vec();
    b.extend(1u32.to_le_bytes());
    b.extend((entries.len() as u32).to_le_bytes());
    for e in entries {
        b.extend((e.len() as u32).to_le_bytes());
        b.extend_from_slice(e);
    }
    b
}

fn kernel_entry() -> Vec<u8> {
    map(&entry(4, 2, 4627))
}

#[test]
fn a_valid_block_parses_with_the_exact_statement_bytes() {
    let publisher = map(&entry(1, 1, 3309));
    let b = block(&[kernel_entry(), publisher]);
    let parsed = SignatureBlock::parse(&b).unwrap();
    let entries: Vec<_> = parsed.entries().collect();
    assert_eq!(entries.len(), 2);
    let k = entries[0];
    assert_eq!((k.role, k.suite), (Role::Kernel, Suite::Oss1R));
    assert_eq!(k.statement.bytes, &map(&statement(4))[..]);
    assert_eq!(k.statement.profile, 6);
    assert_eq!(k.statement.id, "origin.framekernel");
    assert_eq!(k.chain_len, 1);
    assert_eq!(k.sig_mldsa.len(), 4627);
    assert_eq!(entries[1].suite, Suite::Oss1);
    assert_eq!(Role::Kernel.context(), b"origin/onbf/1/origin-kernel");
    assert_eq!(
        Role::DriverAttest.context(),
        b"origin/onbf/1/origin-driver-attest"
    );
    assert_eq!(Suite::Oss1.mldsa_public_key_len(), 1952);
}

#[test]
fn block_framing_is_strict() {
    let good = block(&[kernel_entry()]);
    let mut b = good.clone();
    b[0] = b'X';
    assert_eq!(
        SignatureBlock::parse(&b).unwrap_err(),
        SignatureError::Magic
    );
    let mut b = good.clone();
    b[8] = 2;
    assert_eq!(
        SignatureBlock::parse(&b).unwrap_err(),
        SignatureError::Version(2)
    );
    for count in [0u32, 9] {
        let mut b = good.clone();
        b[12..16].copy_from_slice(&count.to_le_bytes());
        assert_eq!(
            SignatureBlock::parse(&b).unwrap_err(),
            SignatureError::EntryCount(count)
        );
    }
    assert_eq!(
        SignatureBlock::parse(&good[..good.len() - 1]).unwrap_err(),
        SignatureError::Truncated
    );
    let mut b = good.clone();
    b.push(0);
    assert_eq!(
        SignatureBlock::parse(&b).unwrap_err(),
        SignatureError::TrailingData
    );
    // Every strict prefix is refused.
    for len in 0..good.len() {
        assert!(SignatureBlock::parse(&good[..len]).is_err(), "prefix {len}");
    }
}

#[test]
fn entry_rules_are_enforced() {
    let with = |edit: &dyn Fn(&mut Fields)| {
        let mut e = entry(4, 2, 4627);
        edit(&mut e);
        SignatureBlock::parse(&block(&[map(&e)])).unwrap_err()
    };
    let field = |key| SignatureError::Field { entry: 0, key };
    assert_eq!(with(&|e| e[0].1 = uint(0)), field(1), "role 0");
    assert_eq!(with(&|e| e[0].1 = uint(6)), field(1), "unknown role");
    assert_eq!(with(&|e| e[1].1 = uint(3)), field(2), "unknown suite");
    assert_eq!(
        with(&|e| e[1].1 = uint(1)),
        SignatureError::RoleSuite { entry: 0 },
        "the kernel role with OSS-1"
    );
    assert_eq!(
        with(&|e| e[2].1 = bytes(&[0; 31])),
        field(3),
        "key_id length"
    );
    assert_eq!(
        with(&|e| e[3].1 = [head(4, 1), uint(1)].concat()),
        field(4),
        "a certificate that is not a map"
    );
    assert_eq!(
        with(&|e| e[5].1 = bytes(&[0; 63])),
        field(6),
        "Ed25519 length"
    );
    assert_eq!(
        with(&|e| e[6].1 = bytes(&[0; 3309])),
        field(7),
        "ML-DSA length for the suite"
    );
    assert_eq!(
        with(&|e| e[4].1 = map(&statement(5))),
        SignatureError::RoleMismatch { entry: 0 }
    );
    assert_eq!(
        with(&|e| e.push((8, bytes(&[])))),
        SignatureError::Shape { entry: 0 },
        "key 8 is reserved in v1"
    );
    assert_eq!(
        with(&|e| {
            e.remove(5);
        }),
        SignatureError::Shape { entry: 0 }
    );
}

#[test]
fn statement_rules_are_enforced() {
    let with = |edit: &dyn Fn(&mut Fields)| {
        let mut s = statement(4);
        edit(&mut s);
        let mut e = entry(4, 2, 4627);
        e[4].1 = map(&s);
        SignatureBlock::parse(&block(&[map(&e)])).unwrap_err()
    };
    let field = |key: u64| SignatureError::Field {
        entry: 0,
        key: 50 + key,
    };
    assert_eq!(with(&|s| s[0].1 = uint(2)), field(0), "format");
    assert_eq!(
        with(&|s| s[2].1 = bytes(&[0; 32])),
        field(2),
        "digest length"
    );
    assert_eq!(with(&|s| s[3].1 = text("")), field(3), "empty id");
    assert_eq!(with(&|s| s[4].1 = uint(1)), field(4), "version as a number");
    assert_eq!(with(&|s| s[5].1 = uint(0)), field(5), "statement role 0");
    assert_eq!(
        with(&|s| {
            s.remove(6);
        }),
        SignatureError::Shape { entry: 0 }
    );
}

#[test]
fn duplicates_and_non_canonical_entries_are_refused() {
    assert_eq!(
        SignatureBlock::parse(&block(&[kernel_entry(), kernel_entry()])).unwrap_err(),
        SignatureError::Duplicate { entry: 1 }
    );
    // A non-minimal integer head (role 4 written with a one-byte argument).
    let mut e = kernel_entry();
    let at = e.iter().position(|&b| b == 0x04).unwrap();
    e.splice(at..=at, [0x18, 0x04]);
    assert!(matches!(
        SignatureBlock::parse(&block(&[e])).unwrap_err(),
        SignatureError::Ocbor { entry: 0, .. }
    ));
}

#[test]
fn single_byte_flips_never_yield_a_different_valid_reading_unnoticed() {
    // Every flip either is refused or parses to a block whose fields differ; the parser
    // never panics.
    let good = block(&[kernel_entry()]);
    let reference = SignatureBlock::parse(&good).unwrap();
    let r = reference.entries().next().unwrap();
    for i in 0..good.len() {
        let mut b = good.clone();
        b[i] ^= 0x01;
        if let Ok(p) = SignatureBlock::parse(&b) {
            let e = p.entries().next().unwrap();
            assert_ne!(e, r, "flip at {i} read the same entry");
        }
    }
}
