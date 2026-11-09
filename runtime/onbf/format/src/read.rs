//! Explicit little-endian decoding from byte slices (ONBF-R7: no struct casts).

/// A cursor over a byte slice. Every read is bounds-checked and returns `None` at the end.
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Option<&'a [u8; N]> {
        let (head, rest) = self.bytes.split_first_chunk::<N>()?;
        self.bytes = rest;
        Some(head)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.array::<1>().map(|b| u8::from_le_bytes(*b))
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.array().map(|b| u16::from_le_bytes(*b))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.array().map(|b| u32::from_le_bytes(*b))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.array().map(|b| u64::from_le_bytes(*b))
    }
}

/// True if every byte is zero.
pub(crate) fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

/// Rounds `x` up to a multiple of `align`, a power of two. `None` on overflow.
pub(crate) fn align_up(x: u64, align: u64) -> Option<u64> {
    let mask = align.checked_sub(1)?;
    Some(x.checked_add(mask)? & !mask)
}

/// `bytes[start..end]` with `u64` bounds; `None` if out of range.
pub(crate) fn slice(bytes: &[u8], start: u64, end: u64) -> Option<&[u8]> {
    let start = usize::try_from(start).ok()?;
    let end = usize::try_from(end).ok()?;
    bytes.get(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_and_stops_at_end() {
        let mut r = Reader::new(&[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(r.u8(), Some(1));
        assert_eq!(r.u16(), Some(0x0302));
        assert_eq!(r.u32(), Some(0x0706_0504));
        assert_eq!(r.u8(), None);
        assert_eq!(Reader::new(&[1, 2, 3]).u32(), None);
    }

    #[test]
    fn align_up_rounds_and_detects_overflow() {
        assert_eq!(align_up(0, 4096), Some(0));
        assert_eq!(align_up(1, 4096), Some(4096));
        assert_eq!(align_up(4096, 4096), Some(4096));
        assert_eq!(align_up(4097, 8), Some(4104));
        assert_eq!(align_up(u64::MAX, 8), None);
        assert_eq!(align_up(5, 0), None);
    }

    #[test]
    fn slice_rejects_out_of_range() {
        let b = [0u8; 4];
        assert_eq!(slice(&b, 1, 3), Some(&b[1..3]));
        assert_eq!(slice(&b, 3, 5), None);
        assert_eq!(slice(&b, 3, 2), None);
    }
}
