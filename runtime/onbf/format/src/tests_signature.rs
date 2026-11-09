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

/// Errata O1: a statement's `id` is printable ASCII, so the parser needs no NFC tables.
#[test]
fn a_statement_id_must_be_printable_ascii() {
    let parse = |id: &str| {
        let mut s = statement(4);
        s[3].1 = text(id);
        let mut e = entry(4, 2, 4627);
        e[4].1 = map(&s);
        SignatureBlock::parse(&block(&[map(&e)])).map(|_| ())
    };
    for good in ["origin.framekernel", " ", "~", "a b"] {
        assert_eq!(parse(good), Ok(()), "{good:?}");
    }
    let refused = Err(SignatureError::Field { entry: 0, key: 53 });
    // Composed and decomposed forms of one letter, and the control characters on each side
    // of the printable range.
    for bad in ["\u{e9}", "e\u{301}", "\u{7f}", "\u{1f}", "\0", "a\nb"] {
        assert_eq!(parse(bad), refused, "{bad:?}");
    }
}

/// Found by the multi-fault soak: Appendix A limits a chain to four certificates.
#[test]
fn a_chain_of_five_certificates_is_refused() {
    let with_chain = |n: u64| {
        let cert = map(&[(0, uint(1))]);
        let mut e = entry(4, 2, 4627);
        e[3].1 = [head(4, n), cert.repeat(n as usize)].concat();
        map(&e)
    };
    let four = block(&[with_chain(4)]);
    let parsed = SignatureBlock::parse(&four).unwrap();
    assert_eq!(parsed.entries().next().unwrap().chain_len, 4);
    assert_eq!(
        SignatureBlock::parse(&block(&[with_chain(5)])).unwrap_err(),
        SignatureError::Field { entry: 0, key: 4 }
    );
}

/// Found by the multi-fault soak: S3 requires a SemVer 2.0 version, not any text.
#[test]
fn the_statement_version_must_be_semver() {
    let parse = |version: &str| {
        let mut s = statement(4);
        s[4].1 = text(version);
        let mut e = entry(4, 2, 4627);
        e[4].1 = map(&s);
        SignatureBlock::parse(&block(&[map(&e)])).map(|_| ())
    };
    for good in [
        "0.1.0-dev",
        "1.0.0",
        "10.20.30",
        "0.0.0",
        "1.2.3-alpha.1",
        "1.2.3-0",
        "1.2.3-0a",
        "1.2.3--",
        "1.2.3+build",
        "1.2.3+001",
        "1.2.3-rc.1+exp.sha.5114f85",
        "99999999999999999999.0.0",
    ] {
        assert_eq!(parse(good), Ok(()), "{good}");
    }
    let refused = SignatureError::Field { entry: 0, key: 54 };
    for bad in [
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3-",
        "1.2.3-01",
        "1.2.3-a..b",
        "1.2.3+",
        "1.2.3+a..b",
        "1.2.3+a+b",
        "a.b.c",
        "1.2.x",
        "1.2.3-\u{e9}",
        "1.2.3 ",
        "\u{0}.1.0",
        " 1.2.3",
    ] {
        assert_eq!(parse(bad), Err(refused), "{bad:?}");
    }
}

/// Found by the multi-fault soak: Appendix A limits the block to 1 MiB. `Image::parse`
/// applies that to the header's size; the block parser applies it to its own input.
#[test]
fn a_block_over_one_mebibyte_is_refused() {
    let with_fill = |key: u8, fill: usize| {
        let mut e = entry(4, 2, 4627);
        e[2].1 = bytes(&[key; 32]);
        e[3].1 = [head(4, 1), map(&[(0, bytes(&vec![0x44; fill]))])].concat();
        map(&e)
    };
    let (a, b) = (500_000, 500_000);
    let probe = block(&[with_fill(1, a), with_fill(2, b)]).len();
    let exact = a + (1 << 20) - probe;
    let at_limit = block(&[with_fill(1, exact), with_fill(2, b)]);
    assert_eq!(at_limit.len(), 1 << 20);
    assert!(SignatureBlock::parse(&at_limit).is_ok());
    let over = block(&[with_fill(1, exact + 1), with_fill(2, b)]);
    assert_eq!(over.len(), (1 << 20) + 1);
    assert_eq!(
        SignatureBlock::parse(&over).unwrap_err(),
        SignatureError::TooLarge
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
