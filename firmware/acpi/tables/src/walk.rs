//! A walk over a run of structures that each begin with a type and a length, as most tables of
//! ACPI 6.6 §5.2 hold them. The walk refuses a length below the structure's own type and length
//! fields (zero among them, which would never end) and a structure that runs past the end, and
//! stops for good at its first refusal: nothing after a bad length can be found again.

use crate::error::{Error, ErrorKind};
use crate::read::Bytes;

/// How wide a field is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Width {
    /// One byte.
    U8,
    /// Two bytes.
    U16,
    /// Four bytes.
    U32,
}

impl Width {
    const fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
            Self::U32 => 4,
        }
    }
}

/// Where a structure's type and length fields are, and how wide.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shape {
    /// The type field's offset in the structure.
    pub(crate) kind_at: usize,
    /// The type field's width.
    pub(crate) kind: Width,
    /// The length field's offset in the structure.
    pub(crate) len_at: usize,
    /// The length field's width.
    pub(crate) len: Width,
}

impl Shape {
    /// A one-byte type, then a one-byte length (the MADT's and the SRAT's structures, say).
    pub(crate) const BYTE_BYTE: Self = Self {
        kind_at: 0,
        kind: Width::U8,
        len_at: 1,
        len: Width::U8,
    };

    /// A two-byte type, then a two-byte length (the NFIT's structures).
    pub(crate) const WORD_WORD: Self = Self {
        kind_at: 0,
        kind: Width::U16,
        len_at: 2,
        len: Width::U16,
    };

    /// A one-byte type, a byte this walk ignores, then a two-byte length (the PMTT's and the VIOT's
    /// structures).
    pub(crate) const BYTE_PAD_WORD: Self = Self {
        kind_at: 0,
        kind: Width::U8,
        len_at: 2,
        len: Width::U16,
    };

    /// The bytes a structure must have for its type and length fields to be in it.
    const fn fields(&self) -> usize {
        let kind = self.kind_at.saturating_add(self.kind.bytes());
        let len = self.len_at.saturating_add(self.len.bytes());
        if kind > len { kind } else { len }
    }
}

/// One structure, its bytes bounded by its own length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raw<'a> {
    /// Its offset from the table's first byte.
    pub at: usize,
    /// Its type.
    pub kind: u16,
    /// Its bytes, its type and length fields included.
    pub bytes: &'a [u8],
}

/// The walk; an iterator of the structures in order, or of the first refusal.
#[derive(Clone, Debug)]
pub(crate) struct Walk<'a> {
    run: Bytes<'a>,
    at: usize,
    shape: Shape,
    done: bool,
}

impl<'a> Walk<'a> {
    /// A walk over every structure in `run`.
    pub(crate) const fn new(run: Bytes<'a>, shape: Shape) -> Self {
        Self {
            run,
            at: 0,
            shape,
            done: false,
        }
    }

    fn field(&self, at: usize, width: Width) -> Result<u32, Error> {
        let at = self.at.checked_add(at).ok_or_else(|| {
            self.run
                .error(self.at, ErrorKind::Truncated { need: usize::MAX })
        })?;
        match width {
            Width::U8 => self.run.u8(at).map(u32::from),
            Width::U16 => self.run.u16(at).map(u32::from),
            Width::U32 => self.run.u32(at),
        }
    }

    fn step(&mut self) -> Result<Raw<'a>, Error> {
        let kind = self.field(self.shape.kind_at, self.shape.kind)?;
        let length = self.field(self.shape.len_at, self.shape.len)?;
        let kind = u16::try_from(kind).unwrap_or(u16::MAX);
        let length = usize::try_from(length).unwrap_or(usize::MAX);
        if length < self.shape.fields() {
            return Err(self
                .run
                .error(self.at, ErrorKind::Structure { kind, length }));
        }
        let bytes = self.run.get(self.at, length)?;
        let raw = Raw {
            at: self.run.offset(self.at),
            kind,
            bytes,
        };
        // `get` succeeded, so `at + length` is within the run and cannot overflow.
        self.at = self.at.saturating_add(length);
        Ok(raw)
    }
}

impl<'a> Iterator for Walk<'a> {
    type Item = Result<Raw<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.at >= self.run.len() {
            return None;
        }
        let item = self.step();
        self.done = item.is_err();
        Some(item)
    }
}

impl core::iter::FusedIterator for Walk<'_> {}

/// How many places an [`Index`] remembers.
const CHECKPOINTS: usize = 64;

/// A way to find the structure that starts at a given offset, for tables whose structures refer to
/// one another by offset, without allocating: it remembers where every so many structures start,
/// and walks from the nearest of those. A lookup walks at most about `count / 64` structures.
#[derive(Clone, Debug)]
pub(crate) struct Index<'a> {
    run: Bytes<'a>,
    shape: Shape,
    checkpoints: [usize; CHECKPOINTS],
    used: usize,
    count: usize,
}

impl<'a> Index<'a> {
    /// Walks `run` (every structure must be well formed) and indexes it. Refused: more than `max`
    /// structures, which bounds the work every lookup can cost.
    pub(crate) fn build(
        run: Bytes<'a>,
        shape: Shape,
        max: usize,
        field: &'static str,
    ) -> Result<Self, Error> {
        let mut count = 0usize;
        for raw in Walk::new(run, shape) {
            let raw = raw?;
            count = count.saturating_add(1);
            if count > max {
                return Err(Error::new(run.table(), raw.at, ErrorKind::Value { field }));
            }
        }
        let stride = count.div_ceil(CHECKPOINTS).max(1);
        let mut checkpoints = [0usize; CHECKPOINTS];
        let mut used = 0usize;
        let mut at = 0usize;
        for (i, raw) in Walk::new(run, shape).enumerate() {
            let raw = raw?;
            if i.checked_rem(stride) == Some(0)
                && let Some(slot) = checkpoints.get_mut(used)
            {
                *slot = at;
                used = used.saturating_add(1);
            }
            at = at.saturating_add(raw.bytes.len());
        }
        Ok(Self {
            run,
            shape,
            checkpoints,
            used,
            count,
        })
    }

    /// How many structures there are.
    pub(crate) const fn count(&self) -> usize {
        self.count
    }

    /// The structure that starts at `offset` from the table's first byte, if one does.
    pub(crate) fn at(&self, offset: usize) -> Option<Raw<'a>> {
        let local = offset.checked_sub(self.run.base())?;
        let from = self
            .checkpoints
            .get(..self.used)?
            .iter()
            .copied()
            .filter(|&c| c <= local)
            .max()?;
        let mut walk = Walk::new(self.run.rest(from).ok()?, self.shape);
        let mut at = from;
        loop {
            let raw = walk.next()?.ok()?;
            if at == local {
                return Some(raw);
            }
            at = at.saturating_add(raw.bytes.len());
            if at > local {
                return None;
            }
        }
    }
}

/// A structure's bytes as a reader that keeps its offsets in the table, after checking it holds
/// at least `min` bytes (the fixed part its type defines).
pub(crate) fn body<'a>(
    raw: &Raw<'a>,
    table: crate::Signature,
    min: usize,
) -> Result<Bytes<'a>, Error> {
    if raw.bytes.len() < min {
        return Err(Error::new(
            table,
            raw.at,
            ErrorKind::Structure {
                kind: raw.kind,
                length: raw.bytes.len(),
            },
        ));
    }
    Ok(Bytes::at(raw.bytes, table, raw.at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Signature;

    fn walk(bytes: &[u8]) -> Vec<Result<(usize, u16, usize), Error>> {
        Walk::new(Bytes::new(bytes, Signature::new(b"TEST")), Shape::BYTE_BYTE)
            .map(|r| r.map(|raw| (raw.at, raw.kind, raw.bytes.len())))
            .collect()
    }

    #[test]
    fn walks_structures_by_their_lengths() {
        assert_eq!(walk(&[1, 2, 7, 3, 0]), [Ok((0, 1, 2)), Ok((2, 7, 3))]);
        assert!(walk(&[]).is_empty());
    }

    #[test]
    fn a_bad_length_stops_the_walk_for_good() {
        let zero = walk(&[1, 2, 9, 0, 1, 2]);
        assert_eq!(zero.len(), 2);
        assert_eq!(
            zero[1],
            Err(Error::new(
                Signature::new(b"TEST"),
                2,
                ErrorKind::Structure { kind: 9, length: 0 }
            ))
        );
        let one = walk(&[1, 1]);
        assert!(matches!(
            one[0],
            Err(Error {
                kind: ErrorKind::Structure { .. },
                ..
            })
        ));
        let past = walk(&[1, 2, 4, 9, 0]);
        assert!(matches!(
            past[1],
            Err(Error {
                kind: ErrorKind::Truncated { .. },
                ..
            })
        ));
        let lone = walk(&[1, 2, 4]);
        assert!(matches!(
            lone[1],
            Err(Error {
                kind: ErrorKind::Truncated { need: 4 },
                at: 3,
                ..
            })
        ));
    }

    #[test]
    fn wider_shapes_read_their_fields() {
        let mut b = vec![5, 0, 6, 0, 0xAA, 0xBB];
        b.extend_from_slice(&[1, 0xEE, 4, 0]);
        let mut w = Walk::new(Bytes::new(&b, Signature::new(b"TEST")), Shape::WORD_WORD);
        let first = w.next().unwrap().unwrap();
        assert_eq!((first.kind, first.bytes.len()), (5, 6));
        let second = w.next().unwrap().unwrap();
        assert_eq!((second.at, second.kind, second.bytes.len()), (6, 0xEE01, 4));
        let p = [3, 0xFF, 5, 0, 9];
        let mut w = Walk::new(
            Bytes::new(&p, Signature::new(b"TEST")),
            Shape::BYTE_PAD_WORD,
        );
        let s = w.next().unwrap().unwrap();
        assert_eq!((s.kind, s.bytes.len()), (3, 5));
        assert!(w.next().is_none());
    }
}
