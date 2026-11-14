//! **A test-only stand-in** for `origin-acpi-tables` (TASKS 62), until that crate is on this
//! branch (the coordinating session's ruling of 2026-11-14): it checks a definition block's
//! 36-byte header and checksum and hands back the identity and the term list, which is what the
//! interpreter's callers will take from that crate. Deleted when the crate arrives.

use crate::{DefinitionBlock, IntegerWidth};

/// Why a table was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Short,
    Length,
    Checksum,
}

/// The block, and the integer width its revision would set were it the DSDT.
pub(crate) fn block(table: &[u8]) -> Result<(DefinitionBlock<'_>, IntegerWidth), Refused> {
    if table.len() < 36 {
        return Err(Refused::Short);
    }
    let len = u32::from_le_bytes(table[4..8].try_into().unwrap()) as usize;
    if len != table.len() {
        return Err(Refused::Length);
    }
    if table.iter().fold(0u8, |s, &b| s.wrapping_add(b)) != 0 {
        return Err(Refused::Checksum);
    }
    Ok((
        DefinitionBlock {
            signature: table[0..4].try_into().unwrap(),
            oem_id: table[10..16].try_into().unwrap(),
            oem_table_id: table[16..24].try_into().unwrap(),
            aml: &table[36..],
        },
        IntegerWidth::from_dsdt_revision(table[8]),
    ))
}
