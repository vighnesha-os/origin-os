//! Names: segments, AML name strings (ACPI 6.6 §20.2.2) and the text paths the API takes.

use crate::error::{ErrorKind, Result, err};

/// A name segment: four bytes, the first `A`–`Z` or `_`, the others also `0`–`9`.
pub(crate) type Seg = [u8; 4];

const ROOT: u8 = 0x5C;
const PARENT: u8 = 0x5E;
const DUAL: u8 = 0x2E;
const MULTI: u8 = 0x2F;

const fn lead(b: u8) -> bool {
    b.is_ascii_uppercase() || b == b'_'
}

const fn rest(b: u8) -> bool {
    lead(b) || b.is_ascii_digit()
}

/// Whether `s` is a valid segment.
pub(crate) const fn valid(s: &Seg) -> bool {
    let [a, b, c, d] = *s;
    lead(a) && rest(b) && rest(c) && rest(d)
}

/// Whether `b` can start a name string (a prefix or a segment's first byte).
pub(crate) const fn starts_name(b: u8) -> bool {
    b == ROOT || b == PARENT || b == DUAL || b == MULTI || lead(b)
}

/// A name's segments, wherever they are kept.
pub(crate) trait Segs {
    /// From the root (`\`)?
    fn root(&self) -> bool;
    /// How many parent prefixes (`^`).
    fn parents(&self) -> u32;
    /// How many segments.
    fn count(&self) -> u32;
    /// Segment `i`.
    fn seg(&self, i: u32) -> Result<Seg>;
    /// A relative path of one segment, the only kind the search rules apply to (§5.3).
    fn searched(&self) -> bool {
        !self.root() && self.parents() == 0 && self.count() == 1
    }
}

/// A name string as it lies in a table: where its segments start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AmlName {
    pub(crate) root: bool,
    pub(crate) parents: u32,
    /// Offset of the first segment in the term list.
    pub(crate) first: u32,
    pub(crate) count: u32,
}

/// An [`AmlName`] with the bytes it is in.
#[derive(Clone, Copy)]
pub(crate) struct InTable<'a> {
    pub(crate) name: AmlName,
    pub(crate) aml: &'a [u8],
}

impl Segs for InTable<'_> {
    fn root(&self) -> bool {
        self.name.root
    }
    fn parents(&self) -> u32 {
        self.name.parents
    }
    fn count(&self) -> u32 {
        self.name.count
    }
    fn seg(&self, i: u32) -> Result<Seg> {
        let at = i
            .checked_mul(4)
            .and_then(|o| o.checked_add(self.name.first))
            .ok_or(ErrorKind::Name)?;
        seg_at(self.aml, at)
    }
}

fn byte(aml: &[u8], at: u32) -> Result<u8> {
    let i = usize::try_from(at).map_err(|_| ErrorKind::Truncated)?;
    aml.get(i)
        .copied()
        .ok_or_else(|| ErrorKind::Truncated.into())
}

/// The segment at `at`, checked.
pub(crate) fn seg_at(aml: &[u8], at: u32) -> Result<Seg> {
    let s = [
        byte(aml, at)?,
        byte(aml, at.checked_add(1).ok_or(ErrorKind::Truncated)?)?,
        byte(aml, at.checked_add(2).ok_or(ErrorKind::Truncated)?)?,
        byte(aml, at.checked_add(3).ok_or(ErrorKind::Truncated)?)?,
    ];
    if valid(&s) {
        Ok(s)
    } else {
        err(ErrorKind::Name)
    }
}

/// Parses the name string at `at`; returns it and the offset after it. Every segment is checked.
pub(crate) fn parse(aml: &[u8], at: u32) -> Result<(AmlName, u32)> {
    let mut pos = at;
    let mut root = false;
    let mut parents: u32 = 0;
    let next = |p: u32| p.checked_add(1).ok_or(ErrorKind::Truncated);
    if byte(aml, pos)? == ROOT {
        root = true;
        pos = next(pos)?;
    } else {
        while byte(aml, pos)? == PARENT {
            parents = parents.checked_add(1).ok_or(ErrorKind::Name)?;
            pos = next(pos)?;
        }
    }
    let (count, first) = match byte(aml, pos)? {
        0x00 => (0, next(pos)?),
        DUAL => (2, next(pos)?),
        MULTI => {
            let n = byte(aml, next(pos)?)?;
            if n == 0 {
                return err(ErrorKind::Name);
            }
            (
                u32::from(n),
                pos.checked_add(2).ok_or(ErrorKind::Truncated)?,
            )
        }
        b if lead(b) => (1, pos),
        _ => return err(ErrorKind::Name),
    };
    let name = AmlName {
        root,
        parents,
        first,
        count,
    };
    for i in 0..count {
        InTable { name, aml }.seg(i)?;
    }
    let end = count
        .checked_mul(4)
        .and_then(|n| n.checked_add(first))
        .ok_or(ErrorKind::Truncated)?;
    Ok((name, end))
}

/// The most segments a text path given to the API may have.
pub(crate) const TEXT_SEGMENTS: usize = 64;

/// A path given to the API as text: `\_SB.PCI0._HID`, `^FOO`, `_SB.PCI0`. Short segments are
/// padded with `_`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TextName {
    root: bool,
    parents: u32,
    segs: [Seg; TEXT_SEGMENTS],
    count: u32,
}

impl Segs for TextName {
    fn root(&self) -> bool {
        self.root
    }
    fn parents(&self) -> u32 {
        self.parents
    }
    fn count(&self) -> u32 {
        self.count
    }
    fn seg(&self, i: u32) -> Result<Seg> {
        let i = usize::try_from(i).map_err(|_| ErrorKind::Path)?;
        self.segs
            .get(i)
            .copied()
            .ok_or_else(|| ErrorKind::Path.into())
    }
}

impl TextName {
    /// Parses `text`.
    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut bytes = text.as_bytes();
        let mut root = false;
        let mut parents: u32 = 0;
        if let Some((&ROOT, rest)) = bytes.split_first() {
            root = true;
            bytes = rest;
        } else {
            while let Some((&PARENT, rest)) = bytes.split_first() {
                parents = parents.checked_add(1).ok_or(ErrorKind::Path)?;
                bytes = rest;
            }
        }
        let mut segs = [[b'_'; 4]; TEXT_SEGMENTS];
        let mut count: u32 = 0;
        if !bytes.is_empty() {
            for (slot, part) in segs.iter_mut().zip(bytes.split(|&b| b == b'.')) {
                if part.is_empty() || part.len() > 4 {
                    return err(ErrorKind::Path);
                }
                for (d, s) in slot.iter_mut().zip(part) {
                    *d = *s;
                }
                if !valid(slot) {
                    return err(ErrorKind::Path);
                }
                count = count.checked_add(1).ok_or(ErrorKind::Path)?;
            }
            let parts = bytes.split(|&b| b == b'.').count();
            if usize::try_from(count).ok() != Some(parts) {
                return err(ErrorKind::Path);
            }
        }
        Ok(Self {
            root,
            parents,
            segs,
            count,
        })
    }
}
