//! The root tables: the RSDT (ACPI 6.6 §5.2.7) and the XSDT (§5.2.8). The XSDT is the one an OS
//! uses when the firmware gives both (§5.2.8); the RSDT is checked and listed. Each must give a
//! whole number of entries.

use crate::error::{Error, ErrorKind};
use crate::header::{self, HEADER_LEN, Table};

/// A checked RSDT or XSDT.
#[derive(Clone, Copy, Debug)]
pub struct Root<'a> {
    table: Table<'a>,
    width: usize,
}

impl<'a> Root<'a> {
    /// Checks an RSDT (32-bit entries) or an XSDT (64-bit entries).
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check(bytes)?;
        let width = match &table.signature().0 {
            b"RSDT" => 4,
            _ => {
                header::check_signature(bytes, b"XSDT")?;
                8
            }
        };
        let entries = bytes.len().saturating_sub(HEADER_LEN);
        if entries.checked_rem(width) != Some(0) {
            return Err(Error::new(
                table.signature(),
                4,
                ErrorKind::Length {
                    declared: u64::from(table.header().length),
                    available: bytes.len(),
                },
            ));
        }
        Ok(Self { table, width })
    }

    /// The table.
    #[must_use]
    pub const fn table(&self) -> &Table<'a> {
        &self.table
    }

    /// The physical addresses it lists, in order.
    pub fn entries(&self) -> impl Iterator<Item = u64> + 'a {
        let width = self.width;
        self.table
            .bytes()
            .get(HEADER_LEN..)
            .unwrap_or(&[])
            // 4 or 8; `max` lets the compiler see it is never 0, so no panic path is built.
            .chunks_exact(width.max(1))
            .map(|c| {
                let mut v = [0u8; 8];
                for (to, from) in v.iter_mut().zip(c) {
                    *to = *from;
                }
                u64::from_le_bytes(v)
            })
    }

    /// How many entries it lists.
    #[must_use]
    pub fn len(&self) -> usize {
        self.table
            .bytes()
            .len()
            .saturating_sub(HEADER_LEN)
            .checked_div(self.width)
            .unwrap_or(0)
    }

    /// Whether it lists none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        let mut body = Vec::new();
        body.extend_from_slice(&0x1000u64.to_le_bytes());
        body.extend_from_slice(&0x2000u64.to_le_bytes());
        vec![
            table(b"XSDT", 1, &body),
            table(b"RSDT", 1, &[0, 0x10, 0, 0, 0, 0x20, 0, 0]),
        ]
    }

    #[test]
    fn lists_entries_of_either_width() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x1000u64.to_le_bytes());
        body.extend_from_slice(&0x1_0000_2000u64.to_le_bytes());
        let x = table(b"XSDT", 1, &body);
        let root = Root::parse(&x).unwrap();
        assert_eq!(root.entries().collect::<Vec<_>>(), [0x1000, 0x1_0000_2000]);
        assert_eq!(root.len(), 2);
        let r = table(b"RSDT", 1, &[0, 0x10, 0, 0, 0, 0x20, 0, 0]);
        assert_eq!(
            Root::parse(&r).unwrap().entries().collect::<Vec<_>>(),
            [0x1000, 0x2000]
        );
        assert!(Root::parse(&table(b"RSDT", 1, &[])).unwrap().is_empty());
    }

    #[test]
    fn a_partial_entry_is_refused() {
        assert!(matches!(
            Root::parse(&table(b"XSDT", 1, &[0; 12])).unwrap_err().kind,
            ErrorKind::Length { .. }
        ));
        assert!(matches!(
            Root::parse(&table(b"RSDT", 1, &[0; 3])).unwrap_err().kind,
            ErrorKind::Length { .. }
        ));
        assert!(matches!(
            Root::parse(&table(b"FACP", 1, &[])).unwrap_err().kind,
            ErrorKind::Signature { .. }
        ));
    }
}
