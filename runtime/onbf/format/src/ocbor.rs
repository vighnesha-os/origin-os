//! OCBOR-1, the deterministic CBOR profile ONBF manifests use (ONBF §5).
//!
//! It is RFC 8949 "core deterministic encoding" (§4.2.1) restricted to:
//! - unsigned and negative integers, byte strings, UTF-8 text, arrays, maps, `false`,
//!   `true` and `null`;
//! - no floats, tags, indefinite lengths, `undefined` or other simple values;
//! - every integer and length in its shortest form;
//! - map keys that are unsigned integers in strictly ascending order;
//! - depth ≤ 16, ≤ 65,536 items per container, text ≤ 64 KiB, a document ≤ 1 MiB.
//!
//! [`validate`] checks a whole document in one pass with an explicit stack: no recursion,
//! so untrusted nesting cannot exhaust the call stack (ONBF-R10). [`Cursor`] then reads a
//! validated document item by item, for schema decoders such as [`crate::KrnManifest`].

/// Deepest container nesting (§5).
pub const MAX_DEPTH: usize = 16;
/// Most elements in an array, or entries in a map (§5).
pub const MAX_ITEMS: u64 = 65_536;
/// Longest text string in bytes (§5).
pub const MAX_TEXT: u64 = 64 * 1024;
/// Largest document (§5; Appendix A).
pub const MAX_DOCUMENT: usize = 1 << 20;

/// What is wrong with an OCBOR-1 document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OcborErrorKind {
    /// The document is empty.
    Empty,
    /// The document is larger than 1 MiB.
    TooLarge,
    /// The input ends inside an item.
    Truncated,
    /// An integer or length does not use its shortest encoding.
    NonShortest,
    /// An indefinite-length string, array or map, or a stray "break".
    Indefinite,
    /// Additional information 28–30, which RFC 8949 reserves.
    Reserved,
    /// A tag (major type 6).
    Tag,
    /// A floating-point number.
    Float,
    /// A simple value other than `false`, `true` or `null` (including `undefined`).
    Simple,
    /// Containers nested deeper than 16.
    Depth,
    /// An array or map with more than 65,536 elements or entries.
    TooManyItems,
    /// A text string longer than 64 KiB.
    TextTooLong,
    /// A text string that is not valid UTF-8.
    InvalidUtf8,
    /// A map key that is not an unsigned integer.
    KeyNotUnsigned,
    /// A map key not greater than the one before it (also catches duplicates).
    KeysNotAscending,
    /// Bytes after the single top-level item.
    TrailingData,
}

/// An OCBOR-1 violation and the byte offset of the item where it was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OcborError {
    /// Offset of the offending item's first byte.
    pub offset: usize,
    /// The rule it breaks.
    pub kind: OcborErrorKind,
}

impl OcborError {
    const fn at(offset: usize, kind: OcborErrorKind) -> Self {
        Self { offset, kind }
    }
}

/// One item as OCBOR-1 allows it. Containers carry their element (array) or entry (map)
/// count; their contents follow as separate items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item<'a> {
    /// An unsigned integer.
    Unsigned(u64),
    /// A negative integer, whose value is `-1 - n`.
    Negative(u64),
    /// A byte string.
    Bytes(&'a [u8]),
    /// A UTF-8 text string.
    Text(&'a str),
    /// An array with this many elements.
    Array(u64),
    /// A map with this many key/value entries.
    Map(u64),
    /// `false` or `true`.
    Bool(bool),
    /// `null`.
    Null,
}

/// Decodes the item starting at `pos` and returns it with the offset just past its head
/// (and, for strings, past the string body).
fn read_item(bytes: &[u8], pos: usize) -> Result<(Item<'_>, usize), OcborError> {
    use OcborErrorKind as K;
    let err = |kind| OcborError::at(pos, kind);
    let initial = *bytes.get(pos).ok_or(err(K::Truncated))?;
    let major = initial >> 5;
    let info = initial & 0x1F;
    let mut next = pos.checked_add(1).ok_or(err(K::Truncated))?;

    if major == 7 {
        return match info {
            20 => Ok((Item::Bool(false), next)),
            21 => Ok((Item::Bool(true), next)),
            22 => Ok((Item::Null, next)),
            25..=27 => Err(err(K::Float)),
            28..=30 => Err(err(K::Reserved)),
            31 => Err(err(K::Indefinite)),
            _ => Err(err(K::Simple)), // 0–19, 23 (undefined), 24 (one-byte simple)
        };
    }

    // The argument: an integer value, a length or a count, in its shortest form.
    let (arg, width) = match info {
        0..=23 => (u64::from(info), 0usize),
        24..=27 => {
            let width: usize = match info {
                24 => 1,
                25 => 2,
                26 => 4,
                _ => 8,
            };
            let end = next.checked_add(width).ok_or(err(K::Truncated))?;
            let raw = bytes.get(next..end).ok_or(err(K::Truncated))?;
            let value = raw.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
            next = end;
            (value, width)
        }
        28..=30 => return Err(err(K::Reserved)),
        _ => return Err(err(K::Indefinite)),
    };
    let shortest = match width {
        0 => true,
        1 => arg >= 24,
        2 => arg > 0xFF,
        4 => arg > 0xFFFF,
        _ => arg > 0xFFFF_FFFF,
    };
    if !shortest {
        return Err(err(K::NonShortest));
    }

    let item = match major {
        0 => Item::Unsigned(arg),
        1 => Item::Negative(arg),
        2 | 3 => {
            if major == 3 && arg > MAX_TEXT {
                return Err(err(K::TextTooLong));
            }
            let len = usize::try_from(arg).map_err(|_| err(K::Truncated))?;
            let end = next.checked_add(len).ok_or(err(K::Truncated))?;
            let body = bytes.get(next..end).ok_or(err(K::Truncated))?;
            next = end;
            if major == 2 {
                Item::Bytes(body)
            } else {
                Item::Text(core::str::from_utf8(body).map_err(|_| err(K::InvalidUtf8))?)
            }
        }
        4 | 5 => {
            if arg > MAX_ITEMS {
                return Err(err(K::TooManyItems));
            }
            if major == 4 {
                Item::Array(arg)
            } else {
                Item::Map(arg)
            }
        }
        _ => return Err(err(K::Tag)), // major 6
    };
    Ok((item, next))
}

/// One open container during validation.
#[derive(Clone, Copy)]
struct Frame {
    /// Items still to read: elements for an array, keys plus values for a map.
    remaining: u64,
    is_map: bool,
    last_key: Option<u64>,
}

/// Checks that `bytes` is exactly one valid OCBOR-1 item.
pub fn validate(bytes: &[u8]) -> Result<(), OcborError> {
    use OcborErrorKind as K;
    if bytes.is_empty() {
        return Err(OcborError::at(0, K::Empty));
    }
    if bytes.len() > MAX_DOCUMENT {
        return Err(OcborError::at(0, K::TooLarge));
    }
    let mut stack = [Frame {
        remaining: 0,
        is_map: false,
        last_key: None,
    }; MAX_DEPTH];
    let mut depth = 0usize;
    let mut pos = 0usize;
    loop {
        let start = pos;
        let (item, next) = read_item(bytes, pos)?;
        if let Some(frame) = depth.checked_sub(1).and_then(|top| stack.get_mut(top)) {
            // In a map, items alternate key, value; `remaining` is even before each key.
            if frame.is_map && frame.remaining & 1 == 0 {
                let Item::Unsigned(key) = item else {
                    return Err(OcborError::at(start, K::KeyNotUnsigned));
                };
                if frame.last_key.is_some_and(|prev| key <= prev) {
                    return Err(OcborError::at(start, K::KeysNotAscending));
                }
                frame.last_key = Some(key);
            }
            frame.remaining = frame.remaining.saturating_sub(1);
        }
        pos = next;
        let children = match item {
            Item::Array(n) => Some((n, false)),
            Item::Map(n) => Some((n, true)),
            _ => None,
        };
        if let Some((n, is_map)) = children {
            if depth >= MAX_DEPTH {
                return Err(OcborError::at(start, K::Depth));
            }
            if n > 0 {
                let remaining = if is_map { n.saturating_mul(2) } else { n };
                if let Some(slot) = stack.get_mut(depth) {
                    *slot = Frame {
                        remaining,
                        is_map,
                        last_key: None,
                    };
                }
                depth = depth.saturating_add(1);
            }
        }
        while depth > 0
            && stack
                .get(depth.saturating_sub(1))
                .is_some_and(|f| f.remaining == 0)
        {
            depth = depth.saturating_sub(1);
        }
        if depth == 0 {
            break;
        }
    }
    if pos != bytes.len() {
        return Err(OcborError::at(pos, K::TrailingData));
    }
    Ok(())
}

/// Reads a document that [`validate`] accepted, one item at a time.
#[derive(Clone, Debug)]
pub struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// A cursor at the start of `bytes`.
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Offset of the next item.
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Reads the next item. For a container, its contents follow.
    pub fn next_item(&mut self) -> Result<Item<'a>, OcborError> {
        let (item, next) = read_item(self.bytes, self.pos)?;
        self.pos = next;
        Ok(item)
    }

    /// Skips the contents of `item`, which was just read: nothing for a scalar, every
    /// element or entry (recursively, but iteratively) for a container.
    pub fn skip_contents(&mut self, item: Item<'a>) -> Result<(), OcborError> {
        let mut pending: u64 = match item {
            Item::Array(n) => n,
            Item::Map(n) => n.saturating_mul(2),
            _ => 0,
        };
        while pending > 0 {
            pending = pending.saturating_sub(1);
            match self.next_item()? {
                Item::Array(n) => pending = pending.saturating_add(n),
                Item::Map(n) => pending = pending.saturating_add(n.saturating_mul(2)),
                _ => {}
            }
        }
        Ok(())
    }
}
