//! OCBOR-1 (ONBF §5) written from the fuzzer's choices, and every reader of it run.
//!
//! [`item`] writes one item: mostly the forms OCBOR-1 accepts, and sometimes one it refuses (a
//! float, a tag, an indefinite length, a head longer than the shortest, keys out of order or
//! repeated, nesting past the limit, a length past the end), so that each refusal is reached.
//! [`krn_manifest`] writes the KRN manifest's map (`docs/spec/ONBF-manifest-keys.md`), with
//! fields the caller chooses, so that the decoder's checks behind the shape are reached.

use arbitrary::{Result, Unstructured};
use onbf_format::KrnManifest;
use onbf_format::ocbor::{self, Cursor};

/// OCBOR-1's nesting limit (ONBF §5).
const DEPTH: u32 = 16;
/// The most the writer emits: OCBOR-1 allows 1 MiB, and a smaller document keeps a run short.
const MAX_OUT: usize = 64 * 1024;

/// Appends a head of major type `major` (0 to 7) with argument `value`, in the shortest form
/// (RFC 8949 §4.2.1, which OCBOR-1 requires).
pub fn head(out: &mut Vec<u8>, major: u8, value: u64) {
    let m = major << 5;
    if let Ok(v) = u8::try_from(value) {
        if v < 24 {
            out.push(m | v);
        } else {
            out.push(m | 24);
            out.push(v);
        }
    } else if let Ok(v) = u16::try_from(value) {
        out.push(m | 25);
        out.extend_from_slice(&v.to_be_bytes());
    } else if let Ok(v) = u32::try_from(value) {
        out.push(m | 26);
        out.extend_from_slice(&v.to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

/// Appends a head in the 8-byte form whatever `value` is: longer than the shortest unless the
/// value needs it, so refused.
fn long_head(out: &mut Vec<u8>, major: u8, value: u64) {
    out.push((major << 5) | 27);
    out.extend_from_slice(&value.to_be_bytes());
}

/// Appends a byte string.
pub fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    head(out, 2, u64::try_from(b.len()).unwrap_or(u64::MAX));
    out.extend_from_slice(b);
}

/// Appends a text string.
pub fn text(out: &mut Vec<u8>, t: &str) {
    head(out, 3, u64::try_from(t.len()).unwrap_or(u64::MAX));
    out.extend_from_slice(t.as_bytes());
}

/// Appends an unsigned integer.
pub fn uint(out: &mut Vec<u8>, v: u64) {
    head(out, 0, v);
}

/// A count of `n` items, as the `u64` a head carries.
fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Appends one item chosen by `u`, nested at most a little past OCBOR-1's limit.
pub fn item(u: &mut Unstructured<'_>, out: &mut Vec<u8>, depth: u32) -> Result<()> {
    if out.len() >= MAX_OUT || u.is_empty() {
        uint(out, 0);
        return Ok(());
    }
    // Containers stop being chosen once the depth is past the limit.
    let top = if depth > DEPTH + 1 { 5 } else { 15 };
    match u.int_in_range(0..=top)? {
        0 => uint(out, u.arbitrary()?),
        1 => head(out, 1, u.arbitrary()?),
        2 => {
            let n = u.int_in_range(0..=48)?;
            let b = u.bytes(n.min(u.len()))?;
            bytes(out, b);
        }
        3 => {
            let t: &str = u.arbitrary()?;
            text(out, t);
        }
        4 => out.push(*u.choose(&[0xF4, 0xF5, 0xF6])?),
        5 => {
            // A text string that is not UTF-8: refused.
            let n = u.int_in_range(1..=8)?;
            head(out, 3, n);
            for _ in 0..n {
                out.push(u.int_in_range(0x80..=0xFF)?);
            }
        }
        6 | 7 => {
            let n = u.int_in_range(0..=6usize)?;
            head(out, 4, count(n));
            for _ in 0..n {
                item(u, out, depth + 1)?;
            }
        }
        8 | 9 => {
            // A map with strictly ascending unsigned keys, as OCBOR-1 requires.
            let n = u.int_in_range(0..=6usize)?;
            head(out, 5, count(n));
            let mut key: u64 = u.int_in_range(0..=1100)?;
            for _ in 0..n {
                uint(out, key);
                item(u, out, depth + 1)?;
                key = key.saturating_add(u.int_in_range(1..=600)?);
            }
        }
        10 => {
            // A map whose keys repeat, fall, or are not unsigned integers: refused.
            let n = u.int_in_range(2..=4usize)?;
            head(out, 5, count(n));
            for _ in 0..n {
                match u.int_in_range(0..=2)? {
                    0 => uint(out, u.int_in_range(0..=3)?),
                    1 => text(out, "k"),
                    _ => head(out, 1, 0),
                }
                item(u, out, depth + 1)?;
            }
        }
        11 => {
            // A head longer than the shortest.
            let major = u.int_in_range(0..=5)?;
            let v: u8 = u.arbitrary()?;
            long_head(out, major, u64::from(v) % 4);
            if major >= 2 {
                for _ in 0..(v % 4) {
                    if major == 4 || major == 5 {
                        uint(out, 0);
                        if major == 5 {
                            uint(out, 0);
                        }
                    } else {
                        out.push(b'a');
                    }
                }
            }
        }
        12 => {
            // What OCBOR-1 leaves out: floats, tags, indefinite lengths, `undefined`, other
            // simple values and the reserved head forms.
            match u.int_in_range(0..=6)? {
                0 => out.extend_from_slice(&[0xF9, 0x3C, 0x00]),
                1 => out.extend_from_slice(&[0xFB, 0, 0, 0, 0, 0, 0, 0, 0]),
                2 => {
                    out.push(0xC0 | u.int_in_range(0..=23)?);
                    item(u, out, depth + 1)?;
                }
                3 => {
                    out.push(*u.choose(&[0x5F, 0x7F, 0x9F, 0xBF])?);
                    item(u, out, depth + 1)?;
                    out.push(0xFF);
                }
                4 => out.push(0xF7),
                5 => out.push(u.int_in_range(0xE0..=0xF3)?),
                _ => out.push((u.int_in_range(0..=7)? << 5) | u.int_in_range(28..=31)?),
            }
        }
        13 => {
            // A length past the end of the document.
            let major = *u.choose(&[2u8, 3, 4, 5])?;
            head(out, major, u.int_in_range(1..=u64::MAX)?);
        }
        14 => {
            // Nesting past the limit: arrays of one, deeper than 16.
            let levels = u.int_in_range(15..=20)?;
            for _ in 0..levels {
                head(out, 4, 1);
            }
            uint(out, 0);
        }
        _ => {
            // A count of items larger than what follows, in a container that is closed early.
            let major = *u.choose(&[4u8, 5])?;
            head(out, major, u.int_in_range(1..=70_000)?);
            uint(out, 1);
        }
    }
    Ok(())
}

/// The fields of a KRN manifest (ONBF §6.5; `onbf_format::KrnManifest`).
#[derive(Clone, Copy, Debug)]
pub struct KrnFields {
    /// SHA-256 of the SEGMENT contents in table order (key 1).
    pub build_id: [u8; 32],
    /// The entry point's RVA (key 5).
    pub entry: u64,
    /// The boot-info ABI (key 24).
    pub boot_info: (u16, u16),
    /// Required processor features (key 3).
    pub cpu_features: u64,
    /// `cet.shstk` and `cet.ibt` (key 4).
    pub cet: (bool, bool),
}

/// Writes a KRN manifest with `fields`. With probability set by `u`, one field is replaced by
/// an arbitrary item, a key is left out, an unknown key below 1000 is added, or an extension
/// key at 1000 or above is added, so that each of the decoder's refusals is reached.
pub fn krn_manifest(u: &mut Unstructured<'_>, fields: &KrnFields) -> Result<Vec<u8>> {
    // Which field, if any, is spoilt: 0 to 6 are the keys in order, 7 none.
    let spoil = if u.ratio(1, 6)? {
        u.int_in_range(0..=6)?
    } else {
        7
    };
    let leave_out = u.ratio(1, 16)?;
    let unknown = u.ratio(1, 16)?;
    let extension = u.ratio(1, 8)?;
    let mut keys: Vec<(u64, u32)> = vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (24, 6)];
    if leave_out && spoil < 7 {
        keys.retain(|&(_, i)| i != spoil);
    }
    if unknown {
        keys.push((u.int_in_range(6..=23)?, 99));
    }
    if extension {
        keys.push((u.int_in_range(1000..=1200)?, 99));
    }
    keys.sort_by_key(|&(k, _)| k);
    let mut out = Vec::new();
    head(&mut out, 5, count(keys.len()));
    for (key, which) in keys {
        uint(&mut out, key);
        if which == spoil || which == 99 {
            item(u, &mut out, 1)?;
            continue;
        }
        match which {
            0 => uint(&mut out, 1),
            1 => bytes(&mut out, &fields.build_id),
            2 => toolchain(u, &mut out)?,
            3 => uint(&mut out, fields.cpu_features),
            4 => {
                head(&mut out, 5, 2);
                uint(&mut out, 0);
                out.push(if fields.cet.0 { 0xF5 } else { 0xF4 });
                uint(&mut out, 1);
                out.push(if fields.cet.1 { 0xF5 } else { 0xF4 });
            }
            5 => uint(&mut out, fields.entry),
            _ => {
                head(&mut out, 4, 2);
                uint(&mut out, u64::from(fields.boot_info.0));
                uint(&mut out, u64::from(fields.boot_info.1));
            }
        }
    }
    Ok(out)
}

/// The `toolchain` map (key 2): `rustc`, its commit as 40 lowercase hexadecimal digits, `llvm`
/// and the flags' digest.
fn toolchain(u: &mut Unstructured<'_>, out: &mut Vec<u8>) -> Result<()> {
    head(out, 5, 4);
    uint(out, 0);
    text(out, "rustc 1.100.0");
    uint(out, 1);
    let mut commit = String::with_capacity(40);
    for _ in 0..40 {
        let d: u8 = u.int_in_range(0..=15)?;
        commit.push(char::from(if d < 10 { b'0' + d } else { b'a' + d - 10 }));
    }
    text(out, &commit);
    uint(out, 2);
    text(out, "LLVM 22.1.0");
    uint(out, 3);
    let flags: [u8; 32] = u.arbitrary()?;
    bytes(out, &flags);
    Ok(())
}

/// Runs every reader of OCBOR-1 over `bytes`, and judges what the validator accepts.
///
/// # Panics
///
/// On a finding: a validated document whose items cannot all be read, or that is not exactly
/// one item. A panic in a reader is a finding too.
pub fn exercise(bytes: &[u8]) {
    if ocbor::validate(bytes).is_ok() {
        let mut c = Cursor::new(bytes);
        let first = c
            .next_item()
            .expect("OCBOR-1: the first item of a validated document reads");
        c.skip_contents(first)
            .expect("OCBOR-1: every item of a validated document reads");
        assert_eq!(
            c.position(),
            bytes.len(),
            "OCBOR-1: a validated document is exactly one item"
        );
    } else {
        // The cursor is meant for validated documents; on others it must still not panic.
        let mut c = Cursor::new(bytes);
        for _ in 0..1024 {
            let Ok(it) = c.next_item() else { break };
            if c.skip_contents(it).is_err() {
                break;
            }
        }
    }
    let _ = KrnManifest::decode(bytes);
}

/// The `ocbor_built` target: a document written from `data`'s choices, half of the time a KRN
/// manifest, then every reader of it.
pub fn built(data: &[u8]) {
    let mut u = Unstructured::new(data);
    let mut doc = Vec::new();
    let written = match u.ratio(1, 2) {
        Ok(true) => KrnFields::arbitrary_from(&mut u)
            .and_then(|fields| krn_manifest(&mut u, &fields))
            .map(|m| doc = m),
        _ => item(&mut u, &mut doc, 0),
    };
    if written.is_ok() {
        exercise(&doc);
    }
}

impl KrnFields {
    /// Fields chosen by `u`: mostly those a loader accepts (boot-info ABI as built, no CPU
    /// features, IBT off).
    pub fn arbitrary_from(u: &mut Unstructured<'_>) -> Result<Self> {
        Ok(Self {
            build_id: u.arbitrary()?,
            entry: u.arbitrary()?,
            boot_info: if u.ratio(7, 8)? {
                origin_boot_info::VERSION
            } else {
                u.arbitrary()?
            },
            cpu_features: if u.ratio(7, 8)? { 0 } else { u.arbitrary()? },
            cet: (u.arbitrary()?, u.ratio(1, 8)?),
        })
    }
}
