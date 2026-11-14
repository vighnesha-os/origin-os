//! Checked little-endian reads from a table's bytes (ACPI 6.6 §5.2: every number in a table is
//! little-endian). Nothing here indexes, slices or adds without a check, and every failure is a
//! typed [`Error`] naming the table and the offset from its first byte.

use crate::error::{Error, ErrorKind, Signature};

/// A run of a table's bytes, and where it starts in the table, so a refusal inside a nested
/// structure still names its offset from the table's first byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Bytes<'a> {
    bytes: &'a [u8],
    table: Signature,
    base: usize,
}

impl<'a> Bytes<'a> {
    /// The whole of a table.
    pub(crate) const fn new(bytes: &'a [u8], table: Signature) -> Self {
        Self {
            bytes,
            table,
            base: 0,
        }
    }

    /// Bytes that start at `base` in their table: a structure found by a walk, say.
    pub(crate) const fn at(bytes: &'a [u8], table: Signature, base: usize) -> Self {
        Self { bytes, table, base }
    }

    /// The bytes.
    pub(crate) const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// How many bytes there are.
    pub(crate) const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The table these bytes belong to.
    pub(crate) const fn table(&self) -> Signature {
        self.table
    }

    /// Where these bytes start in their table.
    pub(crate) const fn base(&self) -> usize {
        self.base
    }

    /// The offset in the table of `at` in these bytes.
    pub(crate) const fn offset(&self, at: usize) -> usize {
        self.base.saturating_add(at)
    }

    /// A refusal at `at` in these bytes.
    pub(crate) const fn error(&self, at: usize, kind: ErrorKind) -> Error {
        Error::new(self.table, self.offset(at), kind)
    }

    /// The refusal for a field of `len` bytes at `at` that does not fit.
    fn truncated(&self, at: usize, len: usize) -> Error {
        let end = at.saturating_add(len);
        self.error(
            at,
            ErrorKind::Truncated {
                need: self.offset(end),
            },
        )
    }

    /// `len` bytes at `at`.
    pub(crate) fn get(&self, at: usize, len: usize) -> Result<&'a [u8], Error> {
        at.checked_add(len)
            .and_then(|end| self.bytes.get(at..end))
            .ok_or_else(|| self.truncated(at, len))
    }

    /// `len` bytes at `at`, as a run of their own that keeps its place in the table.
    pub(crate) fn sub(&self, at: usize, len: usize) -> Result<Self, Error> {
        Ok(Self {
            bytes: self.get(at, len)?,
            table: self.table,
            base: self.offset(at),
        })
    }

    /// Everything from `at` to the end.
    pub(crate) fn rest(&self, at: usize) -> Result<Self, Error> {
        let len = self
            .len()
            .checked_sub(at)
            .ok_or_else(|| self.truncated(at, 0))?;
        self.sub(at, len)
    }

    /// `N` bytes at `at`.
    pub(crate) fn array<const N: usize>(&self, at: usize) -> Result<[u8; N], Error> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.get(at, N)?);
        Ok(out)
    }

    /// The byte at `at`.
    pub(crate) fn u8(&self, at: usize) -> Result<u8, Error> {
        self.bytes
            .get(at)
            .copied()
            .ok_or_else(|| self.truncated(at, 1))
    }

    /// The 16-bit value at `at`.
    pub(crate) fn u16(&self, at: usize) -> Result<u16, Error> {
        self.array(at).map(u16::from_le_bytes)
    }

    /// The 32-bit value at `at`.
    pub(crate) fn u32(&self, at: usize) -> Result<u32, Error> {
        self.array(at).map(u32::from_le_bytes)
    }

    /// The 64-bit value at `at`.
    pub(crate) fn u64(&self, at: usize) -> Result<u64, Error> {
        self.array(at).map(u64::from_le_bytes)
    }

    /// The byte at `at`, or `None` if these bytes end before it: for a field that a shorter, older
    /// revision of a structure does not have.
    pub(crate) fn opt_u8(&self, at: usize) -> Option<u8> {
        self.bytes.get(at).copied()
    }

    /// As [`Bytes::opt_u8`], for a 32-bit field.
    pub(crate) fn opt_u32(&self, at: usize) -> Option<u32> {
        self.u32(at).ok()
    }

    /// As [`Bytes::opt_u8`], for a 64-bit field.
    pub(crate) fn opt_u64(&self, at: usize) -> Option<u64> {
        self.u64(at).ok()
    }

    /// Refuses with `ErrorKind::TooShort` unless these bytes are at least `need` long.
    pub(crate) fn require(&self, need: usize) -> Result<(), Error> {
        if self.len() < need {
            return Err(self.error(
                0,
                ErrorKind::TooShort {
                    need: self.offset(need),
                },
            ));
        }
        Ok(())
    }

    /// A run of `count` items of `size` bytes each, starting at `at`, which must fit: for an
    /// array whose offset and count come from the firmware. `field` names the count.
    pub(crate) fn array_of(
        &self,
        at: usize,
        count: usize,
        size: usize,
        field: &'static str,
    ) -> Result<Self, Error> {
        let len = count
            .checked_mul(size)
            .ok_or_else(|| self.error(at, ErrorKind::Offset { field }))?;
        self.sub(at, len)
            .map_err(|_| self.error(at, ErrorKind::Offset { field }))
    }
}

/// A GUID as its 16 bytes are stored (ACPI 6.6 §5.2.4: the first three fields little-endian),
/// compared byte for byte.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    /// The GUID written `a-b-c-d-e` in its usual text form, stored as ACPI stores it.
    #[must_use]
    pub const fn from_fields(a: u32, b: u16, c: u16, d: [u8; 8]) -> Self {
        let a = a.to_le_bytes();
        let b = b.to_le_bytes();
        let c = c.to_le_bytes();
        Self([
            a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5],
            d[6], d[7],
        ])
    }
}

impl core::fmt::Debug for Guid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let g = &self.0;
        let a = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
        let b = u16::from_le_bytes([g[4], g[5]]);
        let c = u16::from_le_bytes([g[6], g[7]]);
        write!(f, "{a:08x}-{b:04x}-{c:04x}-{:02x}{:02x}-", g[8], g[9])?;
        for byte in &g[10..] {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Bytes<'_> {
    /// The GUID at `at`.
    pub(crate) fn guid(&self, at: usize) -> Result<Guid, Error> {
        self.array(at).map(Guid)
    }
}

/// The ACPI checksum rule: every byte of the table sums to zero, modulo 256.
#[must_use]
pub fn sums_to_zero(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0
}

/// `a + b`, or a refusal at `at` naming `field` if it overflows.
pub(crate) fn add(b: &Bytes<'_>, x: usize, y: usize, field: &'static str) -> Result<usize, Error> {
    x.checked_add(y)
        .ok_or_else(|| b.error(x, ErrorKind::Offset { field }))
}

/// A firmware `u32` or `u64` as an offset or count; on every host Origin builds for `usize` is 64
/// bits, so this only fails on a value past what any table can hold.
pub(crate) fn index(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_little_endian_and_checked() {
        let b = Bytes::new(&[1, 2, 3, 4, 5, 6, 7, 8, 9], Signature::new(b"TEST"));
        assert_eq!(b.u16(0), Ok(0x0201));
        assert_eq!(b.u32(1), Ok(0x0504_0302));
        assert_eq!(b.u64(1), Ok(0x0908_0706_0504_0302));
        assert_eq!(
            b.u64(2),
            Err(Error::new(
                Signature::new(b"TEST"),
                2,
                ErrorKind::Truncated { need: 10 }
            ))
        );
        assert!(b.get(usize::MAX, 2).is_err());
        assert!(b.u8(9).is_err());
        let s = b.sub(4, 3).unwrap();
        assert_eq!(s.u8(0), Ok(5));
        assert_eq!(s.u8(3).unwrap_err().at, 7, "offsets stay the table's");
        assert_eq!(b.opt_u32(6), None);
        assert!(b.array_of(1, usize::MAX, 2, "count").is_err());
        assert_eq!(b.array_of(1, 4, 2, "count").unwrap().len(), 8);
    }

    #[test]
    fn guids_print_in_their_usual_form() {
        let g = Guid::from_fields(
            0x6619_4C2D,
            0x2E83,
            0x4B8F,
            [0x9D, 0x3E, 0x1B, 0x4E, 0x8A, 0x7C, 0x0D, 0x22],
        );
        assert_eq!(format!("{g:?}"), "66194c2d-2e83-4b8f-9d3e-1b4e8a7c0d22");
        assert_eq!(g.0[0], 0x2D, "the first field is stored little-endian");
    }
}
