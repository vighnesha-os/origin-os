//! An original AML encoder for the tests (ADR-0019 A5), written from ACPI 6.6 chapter 20 alone:
//! each function emits the bytes of one construct, with package lengths computed in their
//! shortest form (or a longer one on request), and records the opcode it emitted so that the
//! coverage test can show every opcode of Table 20.2 was used.

use std::cell::RefCell;
use std::collections::BTreeSet;

/// Bytes of AML.
pub(crate) type B = Vec<u8>;

thread_local! {
    static EMITTED: RefCell<BTreeSet<u16>> = const { RefCell::new(BTreeSet::new()) };
}

/// Records opcode `code`: a one-byte opcode as itself, `0x5B xx` as `0x5Bxx`, `0x92 xx` as
/// `0x92xx`.
pub(crate) fn rec(code: u16) {
    EMITTED.with(|e| {
        e.borrow_mut().insert(code);
    });
}

/// What this thread has emitted, and starts afresh.
pub(crate) fn take_emitted() -> BTreeSet<u16> {
    EMITTED.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

/// A package length for a construct whose body (after the length) is `body` bytes: the shortest
/// encoding, or `min` bytes at least.
pub(crate) fn pkglen(body: usize, min: usize) -> B {
    for n in min.max(1)..=4 {
        let total = body + n;
        let fits = match n {
            1 => total <= 0x3F,
            2 => total <= 0xFFF,
            3 => total <= 0xF_FFFF,
            _ => total <= 0xFFF_FFFF,
        };
        if !fits {
            continue;
        }
        if n == 1 {
            return vec![total as u8];
        }
        let mut out = vec![(((n - 1) as u8) << 6) | (total as u8 & 0x0F)];
        let mut rest = total >> 4;
        for _ in 1..n {
            out.push(rest as u8);
            rest >>= 8;
        }
        return out;
    }
    panic!("package too long for the encoder: {body}");
}

/// `op`, a package length, then `body`.
pub(crate) fn pkg(op: &[u8], body: B) -> B {
    pkg_min(op, body, 1)
}

/// As [`pkg`], with the length encoded in at least `min` bytes.
pub(crate) fn pkg_min(op: &[u8], body: B, min: usize) -> B {
    let mut out = op.to_vec();
    out.extend(pkglen(body.len(), min));
    out.extend(body);
    out
}

fn cat(parts: &[B]) -> B {
    parts.concat()
}

fn op(code: u8, args: &[B]) -> B {
    rec(u16::from(code));
    let mut out = vec![code];
    out.extend(args.concat());
    out
}

fn ext(code: u8, args: &[B]) -> B {
    rec(0x5B00 | u16::from(code));
    let mut out = vec![0x5B, code];
    out.extend(args.concat());
    out
}

// --- Names ----------------------------------------------------------------------------------

fn seg(s: &str) -> [u8; 4] {
    let mut out = [b'_'; 4];
    for (d, c) in out.iter_mut().zip(s.bytes()) {
        *d = c;
    }
    out
}

/// A name string from ASL path text: `\_SB.PCI0`, `^^FOO`, `BAR`; `\` alone is the root.
pub(crate) fn nm(path: &str) -> B {
    let mut out = Vec::new();
    let mut rest = path;
    if let Some(r) = rest.strip_prefix('\\') {
        rec(0x5C);
        out.push(0x5C);
        rest = r;
    }
    while let Some(r) = rest.strip_prefix('^') {
        rec(0x5E);
        out.push(0x5E);
        rest = r;
    }
    let segs: Vec<[u8; 4]> = if rest.is_empty() {
        Vec::new()
    } else {
        rest.split('.').map(seg).collect()
    };
    match segs.len() {
        0 => out.push(0x00),
        1 => {}
        2 => {
            rec(0x2E);
            out.push(0x2E);
        }
        n => {
            rec(0x2F);
            out.push(0x2F);
            out.push(n as u8);
        }
    }
    for s in segs {
        out.extend(s);
    }
    out
}

// --- Data -----------------------------------------------------------------------------------

pub(crate) fn zero() -> B {
    op(0x00, &[])
}
pub(crate) fn one() -> B {
    op(0x01, &[])
}
pub(crate) fn ones() -> B {
    op(0xFF, &[])
}
pub(crate) fn byte(v: u8) -> B {
    op(0x0A, &[vec![v]])
}
pub(crate) fn word(v: u16) -> B {
    op(0x0B, &[v.to_le_bytes().to_vec()])
}
pub(crate) fn dword(v: u32) -> B {
    op(0x0C, &[v.to_le_bytes().to_vec()])
}
pub(crate) fn qword(v: u64) -> B {
    op(0x0E, &[v.to_le_bytes().to_vec()])
}
/// The shortest constant for `v` (never `Ones`, whose value depends on the width).
pub(crate) fn int(v: u64) -> B {
    match v {
        0 => zero(),
        1 => one(),
        v if v <= 0xFF => byte(v as u8),
        v if v <= 0xFFFF => word(v as u16),
        v if v <= 0xFFFF_FFFF => dword(v as u32),
        v => qword(v),
    }
}
pub(crate) fn string(s: &str) -> B {
    let mut out = op(0x0D, &[]);
    out.extend(s.bytes());
    out.push(0);
    out
}
pub(crate) fn revision() -> B {
    ext(0x30, &[])
}
pub(crate) fn buffer(size: B, init: &[u8]) -> B {
    rec(0x11);
    pkg(&[0x11], cat(&[size, init.to_vec()]))
}
pub(crate) fn package(count: u8, elems: &[B]) -> B {
    rec(0x12);
    pkg(&[0x12], cat(&[vec![count], elems.concat()]))
}
pub(crate) fn var_package(count: B, elems: &[B]) -> B {
    rec(0x13);
    pkg(&[0x13], cat(&[count, elems.concat()]))
}
pub(crate) fn local(i: u8) -> B {
    assert!(i < 8);
    op(0x60 + i, &[])
}
pub(crate) fn arg(i: u8) -> B {
    assert!(i < 7);
    op(0x68 + i, &[])
}
pub(crate) fn debug() -> B {
    ext(0x31, &[])
}
/// The null name, as a target.
pub(crate) fn none() -> B {
    vec![0x00]
}

// --- Named objects and namespace modifiers -------------------------------------------------

pub(crate) fn name(path: &str, value: B) -> B {
    op(0x08, &[nm(path), value])
}
pub(crate) fn alias(source: &str, alias: &str) -> B {
    op(0x06, &[nm(source), nm(alias)])
}
pub(crate) fn scope(path: &str, body: &[B]) -> B {
    rec(0x10);
    pkg(&[0x10], cat(&[nm(path), body.concat()]))
}
pub(crate) fn device(path: &str, body: &[B]) -> B {
    rec(0x5B82);
    pkg(&[0x5B, 0x82], cat(&[nm(path), body.concat()]))
}
pub(crate) fn processor(path: &str, id: u8, pblk: u32, len: u8, body: &[B]) -> B {
    rec(0x5B83);
    pkg(
        &[0x5B, 0x83],
        cat(&[
            nm(path),
            vec![id],
            pblk.to_le_bytes().to_vec(),
            vec![len],
            body.concat(),
        ]),
    )
}
pub(crate) fn power_resource(path: &str, level: u8, order: u16, body: &[B]) -> B {
    rec(0x5B84);
    pkg(
        &[0x5B, 0x84],
        cat(&[
            nm(path),
            vec![level],
            order.to_le_bytes().to_vec(),
            body.concat(),
        ]),
    )
}
pub(crate) fn thermal_zone(path: &str, body: &[B]) -> B {
    rec(0x5B85);
    pkg(&[0x5B, 0x85], cat(&[nm(path), body.concat()]))
}
/// `flags`: argument count in bits 0–2, serialized in 3, sync level in 4–7.
pub(crate) fn method(path: &str, flags: u8, body: &[B]) -> B {
    rec(0x14);
    pkg(&[0x14], cat(&[nm(path), vec![flags], body.concat()]))
}
pub(crate) fn external(path: &str, kind: u8, argc: u8) -> B {
    op(0x15, &[nm(path), vec![kind, argc]])
}
pub(crate) fn mutex(path: &str, sync: u8) -> B {
    ext(0x01, &[nm(path), vec![sync]])
}
pub(crate) fn event(path: &str) -> B {
    ext(0x02, &[nm(path)])
}
pub(crate) fn op_region(path: &str, space: u8, offset: B, length: B) -> B {
    ext(0x80, &[nm(path), vec![space], offset, length])
}
pub(crate) fn data_region(path: &str, sig: B, oem: B, table: B) -> B {
    ext(0x88, &[nm(path), sig, oem, table])
}

/// One entry of a field list.
#[derive(Clone)]
pub(crate) enum Fe {
    /// A named field of `bits`.
    Named(&'static str, u32),
    /// Bits skipped (`Offset` compiles to this).
    Reserved(u32),
    /// `AccessAs (type, attribute)`.
    Access(u8, u8),
    /// The extended form, with a length.
    ExtAccess(u8, u8, u8),
    /// `Connection (name)`.
    ConnectName(&'static str),
    /// `Connection (Buffer (n) { … })`.
    ConnectBuffer(Vec<u8>),
}

fn field_list(list: &[Fe]) -> B {
    let mut out = Vec::new();
    for e in list {
        match e {
            Fe::Named(n, bits) => {
                out.extend(seg(n));
                out.extend(bitlen(*bits));
            }
            Fe::Reserved(bits) => {
                out.push(0x00);
                out.extend(bitlen(*bits));
            }
            Fe::Access(t, a) => out.extend([0x01, *t, *a]),
            Fe::ExtAccess(t, a, l) => out.extend([0x03, *t, *a, *l]),
            Fe::ConnectName(n) => {
                out.push(0x02);
                out.extend(nm(n));
            }
            Fe::ConnectBuffer(bytes) => {
                out.push(0x02);
                out.extend(buffer(byte(bytes.len() as u8), bytes));
            }
        }
    }
    out
}

/// A field's bit length, in package-length form (the value itself, not counting its bytes).
fn bitlen(bits: u32) -> B {
    let v = bits as usize;
    if v <= 0x3F {
        return vec![v as u8];
    }
    let n = if v <= 0xFFF {
        2
    } else if v <= 0xF_FFFF {
        3
    } else {
        4
    };
    let mut out = vec![(((n - 1) as u8) << 6) | (v as u8 & 0x0F)];
    let mut rest = v >> 4;
    for _ in 1..n {
        out.push(rest as u8);
        rest >>= 8;
    }
    out
}

pub(crate) fn field(region: &str, flags: u8, list: &[Fe]) -> B {
    rec(0x5B81);
    pkg(
        &[0x5B, 0x81],
        cat(&[nm(region), vec![flags], field_list(list)]),
    )
}
pub(crate) fn index_field(index: &str, data: &str, flags: u8, list: &[Fe]) -> B {
    rec(0x5B86);
    pkg(
        &[0x5B, 0x86],
        cat(&[nm(index), nm(data), vec![flags], field_list(list)]),
    )
}
pub(crate) fn bank_field(region: &str, bank: &str, value: B, flags: u8, list: &[Fe]) -> B {
    rec(0x5B87);
    pkg(
        &[0x5B, 0x87],
        cat(&[nm(region), nm(bank), value, vec![flags], field_list(list)]),
    )
}
pub(crate) fn create_bit_field(src: B, index: B, n: &str) -> B {
    op(0x8D, &[src, index, nm(n)])
}
pub(crate) fn create_byte_field(src: B, index: B, n: &str) -> B {
    op(0x8C, &[src, index, nm(n)])
}
pub(crate) fn create_word_field(src: B, index: B, n: &str) -> B {
    op(0x8B, &[src, index, nm(n)])
}
pub(crate) fn create_dword_field(src: B, index: B, n: &str) -> B {
    op(0x8A, &[src, index, nm(n)])
}
pub(crate) fn create_qword_field(src: B, index: B, n: &str) -> B {
    op(0x8F, &[src, index, nm(n)])
}
pub(crate) fn create_field(src: B, bit: B, bits: B, n: &str) -> B {
    ext(0x13, &[src, bit, bits, nm(n)])
}

// --- Statements -----------------------------------------------------------------------------

pub(crate) fn if_(pred: B, body: &[B]) -> B {
    rec(0xA0);
    pkg(&[0xA0], cat(&[pred, body.concat()]))
}
pub(crate) fn else_(body: &[B]) -> B {
    rec(0xA1);
    pkg(&[0xA1], body.concat())
}
pub(crate) fn if_else(pred: B, body: &[B], other: &[B]) -> B {
    cat(&[if_(pred, body), else_(other)])
}
pub(crate) fn while_(pred: B, body: &[B]) -> B {
    rec(0xA2);
    pkg(&[0xA2], cat(&[pred, body.concat()]))
}
pub(crate) fn ret(v: B) -> B {
    op(0xA4, &[v])
}
pub(crate) fn brk() -> B {
    op(0xA5, &[])
}
pub(crate) fn cont() -> B {
    op(0x9F, &[])
}
pub(crate) fn noop() -> B {
    op(0xA3, &[])
}
pub(crate) fn breakpoint() -> B {
    op(0xCC, &[])
}
pub(crate) fn notify(obj: B, v: B) -> B {
    op(0x86, &[obj, v])
}
pub(crate) fn fatal(kind: u8, code: u32, arg: B) -> B {
    ext(0x32, &[vec![kind], code.to_le_bytes().to_vec(), arg])
}
pub(crate) fn sleep(ms: B) -> B {
    ext(0x22, &[ms])
}
pub(crate) fn stall(us: B) -> B {
    ext(0x21, &[us])
}
pub(crate) fn signal(e: B) -> B {
    ext(0x24, &[e])
}
pub(crate) fn reset(e: B) -> B {
    ext(0x26, &[e])
}
pub(crate) fn release(m: B) -> B {
    ext(0x27, &[m])
}
pub(crate) fn acquire(m: B, timeout: u16) -> B {
    ext(0x23, &[m, timeout.to_le_bytes().to_vec()])
}
pub(crate) fn wait(e: B, timeout: B) -> B {
    ext(0x25, &[e, timeout])
}
pub(crate) fn load_op(n: &str, target: B) -> B {
    ext(0x20, &[nm(n), target])
}
pub(crate) fn load_table(args: [B; 6]) -> B {
    ext(0x1F, &args)
}
pub(crate) fn timer() -> B {
    ext(0x33, &[])
}

// --- Expressions ----------------------------------------------------------------------------

pub(crate) fn store(v: B, to: B) -> B {
    op(0x70, &[v, to])
}
pub(crate) fn ref_of(x: B) -> B {
    op(0x71, &[x])
}
pub(crate) fn deref_of(x: B) -> B {
    op(0x83, &[x])
}
pub(crate) fn cond_ref_of(x: B, t: B) -> B {
    ext(0x12, &[x, t])
}
pub(crate) fn copy_object(v: B, to: B) -> B {
    op(0x9D, &[v, to])
}
pub(crate) fn index(src: B, i: B, t: B) -> B {
    op(0x88, &[src, i, t])
}
pub(crate) fn size_of(x: B) -> B {
    op(0x87, &[x])
}
pub(crate) fn object_type(x: B) -> B {
    op(0x8E, &[x])
}
pub(crate) fn increment(x: B) -> B {
    op(0x75, &[x])
}
pub(crate) fn decrement(x: B) -> B {
    op(0x76, &[x])
}
/// A two-operand integer operator with a target: `Add` 0x72, `Subtract` 0x74, … `Mod` 0x85.
pub(crate) fn bin(code: u8, a: B, b: B, t: B) -> B {
    op(code, &[a, b, t])
}
pub(crate) fn add(a: B, b: B, t: B) -> B {
    bin(0x72, a, b, t)
}
pub(crate) fn concat(a: B, b: B, t: B) -> B {
    bin(0x73, a, b, t)
}
pub(crate) fn divide(a: B, b: B, rem: B, q: B) -> B {
    op(0x78, &[a, b, rem, q])
}
/// A one-operand operator with a target: `Not` 0x80 … `ToInteger` 0x99, `FromBCD`/`ToBCD` by
/// [`un_ext`].
pub(crate) fn un(code: u8, a: B, t: B) -> B {
    op(code, &[a, t])
}
pub(crate) fn un_ext(code: u8, a: B, t: B) -> B {
    ext(code, &[a, t])
}
/// A comparison or logical operator: `LAnd` 0x90 … `LLess` 0x95.
pub(crate) fn logic(code: u8, a: B, b: B) -> B {
    op(code, &[a, b])
}
pub(crate) fn lnot(a: B) -> B {
    op(0x92, &[a])
}
/// `LNotEqual` (0x93), `LLessEqual` (0x94), `LGreaterEqual` (0x95) as `LNot` pairs.
pub(crate) fn lnot_pair(second: u8, a: B, b: B) -> B {
    rec(0x9200 | u16::from(second));
    let mut out = vec![0x92, second];
    out.extend(a);
    out.extend(b);
    out
}
pub(crate) fn match_(pkg: B, op1: u8, m1: B, op2: u8, m2: B, start: B) -> B {
    op(0x89, &[pkg, vec![op1], m1, vec![op2], m2, start])
}
pub(crate) fn mid(src: B, i: B, len: B, t: B) -> B {
    op(0x9E, &[src, i, len, t])
}
pub(crate) fn to_string(src: B, len: B, t: B) -> B {
    op(0x9C, &[src, len, t])
}
/// A method call: the name, then its arguments.
pub(crate) fn call(path: &str, args: &[B]) -> B {
    cat(&[nm(path), args.concat()])
}

// --- Tables ---------------------------------------------------------------------------------

/// A whole definition block: header (revision `rev`) and checksum.
pub(crate) fn table(sig: &[u8; 4], rev: u8, body: &[B]) -> Vec<u8> {
    let body = body.concat();
    let len = 36 + body.len();
    let mut t = Vec::with_capacity(len);
    t.extend(sig);
    t.extend((len as u32).to_le_bytes());
    t.push(rev);
    t.push(0);
    t.extend(*b"ORIGIN");
    t.extend(*b"AMLTEST ");
    t.extend(1u32.to_le_bytes());
    t.extend(*b"ORGN");
    t.extend(1u32.to_le_bytes());
    t.extend(body);
    let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
    t
}
