//! Definition blocks: the DSDT, SSDTs and PSDTs (ACPI 6.6 §5.2.11). The header is checked and the
//! AML after it is handed on as bytes; nothing here interprets it (that is ADR-0019's area B, TASKS
//! item 63).

use crate::error::Error;
use crate::header::{self, HEADER_LEN, Table};

/// Which definition block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The differentiated system description table, loaded first.
    Dsdt,
    /// A secondary one, loaded after the DSDT in the XSDT's order.
    Ssdt,
    /// ACPI 1.0's persistent table, which ACPI 6.6 tells the OS to treat as an SSDT.
    Psdt,
}

/// A checked definition block.
#[derive(Clone, Copy, Debug)]
pub struct DefinitionBlock<'a> {
    table: Table<'a>,
    /// Which block it is.
    pub kind: Kind,
}

impl<'a> DefinitionBlock<'a> {
    /// Checks a definition block's header, length and checksum.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check(bytes)?;
        let kind = match &table.signature().0 {
            b"DSDT" => Kind::Dsdt,
            b"PSDT" => Kind::Psdt,
            _ => {
                header::check_signature(bytes, b"SSDT")?;
                Kind::Ssdt
            }
        };
        Ok(Self { table, kind })
    }

    /// The table.
    #[must_use]
    pub const fn table(&self) -> &Table<'a> {
        &self.table
    }

    /// The AML after the header.
    #[must_use]
    pub fn aml(&self) -> &'a [u8] {
        self.table.bytes().get(HEADER_LEN..).unwrap_or(&[])
    }

    /// The width in bits of the AML's integers: 32 for a block of revision 0 or 1 (ACPI 1.0),
    /// else 64 (§5.2.11.1). For the DSDT, the interpreter's width for every block.
    #[must_use]
    pub const fn integer_width(&self) -> u8 {
        if self.table.revision() < 2 { 32 } else { 64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::table;

    #[test]
    fn the_aml_is_handed_on_unread() {
        let t = table(b"DSDT", 2, &[0x10, 0x20, 0x30]);
        let d = DefinitionBlock::parse(&t).unwrap();
        assert_eq!(
            (d.kind, d.integer_width(), d.aml()),
            (Kind::Dsdt, 64, &[0x10, 0x20, 0x30][..])
        );
        let t = table(b"SSDT", 1, &[]);
        let s = DefinitionBlock::parse(&t).unwrap();
        assert_eq!(
            (s.kind, s.integer_width(), s.aml().len()),
            (Kind::Ssdt, 32, 0)
        );
        assert_eq!(
            DefinitionBlock::parse(&table(b"PSDT", 1, &[1]))
                .unwrap()
                .kind,
            Kind::Psdt
        );
        assert!(DefinitionBlock::parse(&table(b"APIC", 1, &[])).is_err());
    }
}
