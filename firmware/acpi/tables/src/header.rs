//! The system description table header (ACPI 6.6 §5.2.6), and the check every table with one
//! passes before anything else of it is read: its length is the bytes given, and they sum to zero.

use crate::error::{Error, ErrorKind, Signature};
use crate::read::{Bytes, sums_to_zero};

/// Bytes in a system description table header.
pub const HEADER_LEN: usize = 36;

/// A system description table header (§5.2.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// The table's signature.
    pub signature: Signature,
    /// The table's length in bytes, the header included.
    pub length: u32,
    /// The table's revision: the layout of what follows the header.
    pub revision: u8,
    /// The byte that makes the table sum to zero.
    pub checksum: u8,
    /// The OEM's identifier.
    pub oem_id: [u8; 6],
    /// The OEM's identifier for this table, or for the RSDT and XSDT, its model.
    pub oem_table_id: [u8; 8],
    /// The OEM's revision of the table for that table ID.
    pub oem_revision: u32,
    /// The vendor of the tool that built the table.
    pub creator_id: [u8; 4],
    /// That tool's revision.
    pub creator_revision: u32,
}

impl Header {
    /// Reads the 36 bytes of a header, checking nothing but that they are there: the length and
    /// checksum need the whole table, which [`check`] takes.
    pub fn read(bytes: &[u8]) -> Result<Self, Error> {
        let signature = Signature(bytes.first_chunk::<4>().copied().unwrap_or(*b"????"));
        let b = Bytes::new(bytes, signature);
        b.require(HEADER_LEN)?;
        Ok(Self {
            signature,
            length: b.u32(4)?,
            revision: b.u8(8)?,
            checksum: b.u8(9)?,
            oem_id: b.array(10)?,
            oem_table_id: b.array(16)?,
            oem_revision: b.u32(24)?,
            creator_id: b.array(28)?,
            creator_revision: b.u32(32)?,
        })
    }
}

/// A table whose header, length and checksum have been checked.
#[derive(Clone, Copy, Debug)]
pub struct Table<'a> {
    header: Header,
    bytes: &'a [u8],
}

impl<'a> Table<'a> {
    /// The checked header.
    #[must_use]
    pub const fn header(&self) -> &Header {
        &self.header
    }

    /// Every byte of the table, the header included.
    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// The table's signature.
    #[must_use]
    pub const fn signature(&self) -> Signature {
        self.header.signature
    }

    /// The table's revision.
    #[must_use]
    pub const fn revision(&self) -> u8 {
        self.header.revision
    }

    /// The table's bytes as a checked reader.
    pub(crate) const fn reader(&self) -> Bytes<'a> {
        Bytes::new(self.bytes, self.header.signature)
    }
}

/// Checks a whole table: a header, a length that is exactly the bytes given, and a checksum
/// (§5.2.6: the entire table sums to zero).
pub fn check(bytes: &[u8]) -> Result<Table<'_>, Error> {
    let header = Header::read(bytes)?;
    let available = bytes.len();
    let declared = usize::try_from(header.length).unwrap_or(usize::MAX);
    if declared < HEADER_LEN || declared != available {
        return Err(Error::new(
            header.signature,
            4,
            ErrorKind::Length {
                declared: u64::from(header.length),
                available,
            },
        ));
    }
    if !sums_to_zero(bytes) {
        return Err(Error::new(header.signature, 9, ErrorKind::Checksum));
    }
    Ok(Table { header, bytes })
}

/// [`check`], and the signature must be `expected`: for a table reached through a pointer that
/// names what it points to (the FADT's DSDT, say).
pub fn check_signature<'a>(bytes: &'a [u8], expected: &[u8; 4]) -> Result<Table<'a>, Error> {
    let header = Header::read(bytes)?;
    if !header.signature.is(expected) {
        return Err(Error::new(
            Signature::new(expected),
            0,
            ErrorKind::Signature {
                found: header.signature,
            },
        ));
    }
    check(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::table;

    #[test]
    fn a_table_is_checked_whole() {
        let t = table(b"SBST", 1, &[0u8; 12]);
        let checked = check(&t).unwrap();
        assert_eq!(checked.signature(), Signature::new(b"SBST"));
        assert_eq!(checked.header().length, 48);
        assert_eq!(checked.header().oem_id, *b"ORIGIN");
        assert!(check_signature(&t, b"SBST").is_ok());
        assert_eq!(
            check_signature(&t, b"ECDT").unwrap_err().kind,
            ErrorKind::Signature {
                found: Signature::new(b"SBST")
            }
        );
    }

    #[test]
    fn length_and_checksum_are_refused_by_name() {
        let t = table(b"SBST", 1, &[0u8; 12]);
        assert_eq!(
            check(&t[..47]).unwrap_err(),
            Error::new(
                Signature::new(b"SBST"),
                4,
                ErrorKind::Length {
                    declared: 48,
                    available: 47
                }
            )
        );
        let mut long = t.clone();
        long.push(0);
        assert!(matches!(
            check(&long).unwrap_err().kind,
            ErrorKind::Length { .. }
        ));
        let mut small = t.clone();
        small[4..8].copy_from_slice(&35u32.to_le_bytes());
        assert!(matches!(
            check(&small[..35]).unwrap_err().kind,
            ErrorKind::TooShort { .. }
        ));
        assert!(matches!(
            check(&small).unwrap_err().kind,
            ErrorKind::Length { .. }
        ));
        let mut bad = t.clone();
        bad[40] ^= 1;
        assert_eq!(check(&bad).unwrap_err().kind, ErrorKind::Checksum);
        assert!(check(&[]).is_err());
        assert!(check(&t[..3]).is_err());
    }
}
