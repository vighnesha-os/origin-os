//! A canonical OCBOR-1 encoder (ONBF §5): RFC 8949 §4.2.1 core deterministic encoding.
//!
//! - Every integer, length and count uses its shortest head (RFC 8949 §4.2.1).
//! - Map keys are unsigned integers. A [`Value::Map`] is a `BTreeMap`, so keys are unique
//!   and are written in ascending numeric order, which for unsigned integers is also the
//!   bytewise order of their shortest encodings.
//! - Only the types a manifest writer needs are offered: unsigned integers, byte strings,
//!   UTF-8 text, arrays, maps and booleans. Floats, tags, indefinite lengths and
//!   `undefined` cannot be expressed at all.
//!
//! The OCBOR-1 size limits (depth 16, 65,536 items, text 64 KiB, document 1 MiB) are not
//! checked while encoding: the packer validates every document it writes with
//! `onbf_format::ocbor::validate` before using it.

use std::collections::BTreeMap;

/// An OCBOR-1 data item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// Major type 0.
    Unsigned(u64),
    /// Major type 2.
    Bytes(Vec<u8>),
    /// Major type 3. Identifiers must be NFC (§5); the KRN manifest uses ASCII only.
    Text(String),
    /// Major type 4.
    Array(Vec<Value>),
    /// Major type 5, with unsigned-integer keys.
    Map(BTreeMap<u64, Value>),
    /// `false` (0xF4) or `true` (0xF5).
    Bool(bool),
}

const MAJOR_UNSIGNED: u8 = 0;
const MAJOR_BYTES: u8 = 2;
const MAJOR_TEXT: u8 = 3;
const MAJOR_ARRAY: u8 = 4;
const MAJOR_MAP: u8 = 5;
const FALSE: u8 = 0xF4;
const TRUE: u8 = 0xF5;

/// Writes a head: major type and argument, in the shortest form.
fn head(out: &mut Vec<u8>, major: u8, argument: u64) {
    let m = major << 5;
    if let Ok(small) = u8::try_from(argument)
        && small < 24
    {
        out.push(m | small);
    } else if let Ok(v) = u8::try_from(argument) {
        out.push(m | 24);
        out.push(v);
    } else if let Ok(v) = u16::try_from(argument) {
        out.push(m | 25);
        out.extend_from_slice(&v.to_be_bytes());
    } else if let Ok(v) = u32::try_from(argument) {
        out.push(m | 26);
        out.extend_from_slice(&v.to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&argument.to_be_bytes());
    }
}

/// A length or count as a head argument. Lossless on every 64-bit host.
fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl Value {
    /// The canonical encoding of this item.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }

    /// Appends the canonical encoding of this item to `out`. Recursion depth equals the
    /// nesting depth of a value the packer built itself, never of untrusted input.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::Unsigned(v) => head(out, MAJOR_UNSIGNED, *v),
            Self::Bytes(b) => {
                head(out, MAJOR_BYTES, count(b.len()));
                out.extend_from_slice(b);
            }
            Self::Text(t) => {
                head(out, MAJOR_TEXT, count(t.len()));
                out.extend_from_slice(t.as_bytes());
            }
            Self::Array(items) => {
                head(out, MAJOR_ARRAY, count(items.len()));
                for item in items {
                    item.encode_into(out);
                }
            }
            Self::Map(entries) => {
                head(out, MAJOR_MAP, count(entries.len()));
                for (key, value) in entries {
                    head(out, MAJOR_UNSIGNED, *key);
                    value.encode_into(out);
                }
            }
            Self::Bool(b) => out.push(if *b { TRUE } else { FALSE }),
        }
    }
}
