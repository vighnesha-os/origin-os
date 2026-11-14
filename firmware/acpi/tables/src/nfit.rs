//! The NVDIMM Firmware Interface Table, signature `NFIT` (ACPI 6.6 §5.2.26).
//!
//! Its eight structure types are decoded, and the references between them checked: a region
//! mapping's SPA range, control region and interleave indexes each name a structure of their kind,
//! and a block data window names a control region with as many windows. Types 8 and above are
//! reserved, walked and listed.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::Guid;
use crate::walk::{Raw, Shape, Walk, body};

/// Where the first structure is.
const ENTRIES: usize = 40;

/// The most structures an NFIT may hold here. The references between structures are checked pair
/// by pair, so this bounds that work on a hostile table; an NFIT describes a few structures for
/// each NVDIMM, and a machine has tens of them.
pub const MAX_STRUCTURES: usize = 1024;

/// The address range types §5.2.26.2 names, by GUID.
pub mod range_type {
    use crate::read::Guid;

    /// Persistent memory.
    pub const PERSISTENT_MEMORY: Guid = Guid::from_fields(
        0x66F0_D379,
        0xB4F3,
        0x4074,
        [0xAC, 0x43, 0x0D, 0x33, 0x18, 0xB7, 0x8C, 0xDB],
    );
    /// An NVDIMM control region.
    pub const CONTROL_REGION: Guid = Guid::from_fields(
        0x92F7_01F6,
        0x13B4,
        0x405D,
        [0x91, 0x0B, 0x29, 0x93, 0x67, 0xE8, 0x23, 0x4C],
    );
    /// An NVDIMM block data window region.
    pub const BLOCK_DATA_WINDOW: Guid = Guid::from_fields(
        0x91AF_0530,
        0x5D86,
        0x470E,
        [0xA6, 0xB0, 0x0A, 0x2D, 0xB9, 0x40, 0x82, 0x49],
    );
    /// A RAM disk holding a disk image, volatile.
    pub const VIRTUAL_DISK_VOLATILE: Guid = Guid::from_fields(
        0x77AB_535A,
        0x45FC,
        0x624B,
        [0x55, 0x60, 0xF7, 0xB2, 0x81, 0xD1, 0xF9, 0x6E],
    );
    /// A RAM disk holding a CD image, volatile.
    pub const VIRTUAL_CD_VOLATILE: Guid = Guid::from_fields(
        0x3D5A_BD30,
        0x4175,
        0x87CE,
        [0x6D, 0x64, 0xD2, 0xAD, 0xE5, 0x23, 0xC4, 0xBB],
    );
    /// A RAM disk holding a disk image, persistent.
    pub const VIRTUAL_DISK_PERSISTENT: Guid = Guid::from_fields(
        0x5CEA_02C9,
        0x4D07,
        0x69D3,
        [0x26, 0x9F, 0x44, 0x96, 0xFB, 0xE0, 0x96, 0xF9],
    );
    /// A RAM disk holding a CD image, persistent.
    pub const VIRTUAL_CD_PERSISTENT: Guid = Guid::from_fields(
        0x0801_8188,
        0x42CD,
        0xBB48,
        [0x10, 0x0F, 0x53, 0x87, 0xD5, 0x3D, 0xED, 0x3D],
    );
}

/// One NFIT structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NfitEntry<'a> {
    /// A system physical address range (type 0).
    SpaRange {
        /// Its index, which region mappings name it by.
        index: u16,
        /// Its flags: bit 1 says the proximity domain is valid, bit 2 the cookie.
        flags: u16,
        /// Its proximity domain.
        proximity_domain: u32,
        /// What the range is.
        range_type: Guid,
        /// Its base.
        base: u64,
        /// Its length.
        length: u64,
        /// Its `EFI_MEMORY_*` attributes.
        attributes: u64,
        /// The firmware's cookie, if the structure holds one and its flag says it is valid.
        cookie: Option<u64>,
    },
    /// How an NVDIMM's region maps into a range (type 1).
    RegionMapping {
        /// The NVDIMM's `_ADR`.
        device_handle: u32,
        /// The NVDIMM's SMBIOS handle.
        physical_id: u16,
        /// The region's ID within its NVDIMM.
        region_id: u16,
        /// The SPA range it is mapped into, or 0 for none.
        spa_index: u16,
        /// Its control region.
        control_index: u16,
        /// The region's size.
        size: u64,
        /// Where it starts in its interleave set.
        offset: u64,
        /// Its base inside the NVDIMM.
        dpa: u64,
        /// Its interleave structure, or 0.
        interleave_index: u16,
        /// How many NVDIMMs share the interleave set.
        interleave_ways: u16,
        /// The NVDIMM's state flags.
        state: u16,
    },
    /// An interleave pattern (type 2).
    Interleave {
        /// Its index.
        index: u16,
        /// The bytes per line.
        line_size: u32,
        /// Each line's offset, in lines, four bytes each.
        lines: &'a [u8],
    },
    /// SMBIOS data the boot table lacks (type 3).
    Smbios(&'a [u8]),
    /// An NVDIMM control region (type 4).
    ControlRegion {
        /// Its index.
        index: u16,
        /// The module's vendor.
        vendor_id: u16,
        /// The module's device.
        device_id: u16,
        /// The module's serial number.
        serial: u32,
        /// The interface its region speaks.
        format_interface: u16,
        /// How many block control windows it has.
        windows: u16,
    },
    /// A block data window region (type 5).
    BlockDataWindow {
        /// The control region it belongs to.
        control_index: u16,
        /// How many windows.
        windows: u16,
        /// The first window's offset.
        offset: u64,
        /// Each window's size.
        size: u64,
        /// The block-accessible capacity.
        capacity: u64,
    },
    /// Addresses whose write flushes an NVDIMM's buffers (type 6).
    FlushHint {
        /// The NVDIMM.
        device_handle: u32,
        /// The addresses, eight bytes each.
        addresses: &'a [u8],
    },
    /// What the platform guarantees (type 7).
    PlatformCapabilities {
        /// The highest capability bit defined.
        highest_valid: u8,
        /// The capability bits, those above `highest_valid` cleared.
        capabilities: u32,
    },
    /// A type ACPI 6.6 reserves (8 and above).
    Reserved(Raw<'a>),
}

/// The checked NFIT.
#[derive(Clone, Copy, Debug)]
pub struct Nfit<'a> {
    table: Table<'a>,
}

impl<'a> Nfit<'a> {
    /// Checks the table: every structure and the references between them. Refused besides each
    /// structure's own length: a range that wraps; two ranges, interleaves or control regions with
    /// one index; an interleave index of 0; a region mapping whose SPA range, control region or
    /// interleave index names nothing of that kind, or whose interleave index and ways are a pair
    /// §5.2.26.3 does not allow; two regions with one ID on one NVDIMM; a control region with
    /// windows that is not 80 bytes; a block data window whose control region is not there or has
    /// a different number of windows; a capability that needs another the platform lacks.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"NFIT")?;
        table.reader().require(ENTRIES)?;
        let nfit = Self { table };
        for (i, entry) in nfit.entries().enumerate() {
            let (at, entry) = entry?;
            if i >= MAX_STRUCTURES {
                return Err(Error::new(
                    table.signature(),
                    at,
                    ErrorKind::Value {
                        field: "more structures than the NFIT's checks take",
                    },
                ));
            }
            nfit.check_references(i, at, &entry)?;
        }
        Ok(nfit)
    }

    /// Every structure with its offset, in order.
    pub fn entries(&self) -> impl Iterator<Item = Result<(usize, NfitEntry<'a>), Error>> + 'a {
        let signature = self.table.signature();
        self.table
            .reader()
            .rest(ENTRIES)
            .ok()
            .into_iter()
            .flat_map(|run| Walk::new(run, Shape::WORD_WORD))
            .map(move |raw| {
                let raw = raw?;
                Ok((raw.at, decode(&raw, signature)?))
            })
    }

    /// Whether some structure other than the `skip`th satisfies `matches`.
    fn any(&self, skip: usize, matches: impl Fn(&NfitEntry<'a>) -> bool) -> Result<bool, Error> {
        for (i, entry) in self.entries().enumerate() {
            let (_, entry) = entry?;
            if i != skip && matches(&entry) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn check_references(&self, i: usize, at: usize, entry: &NfitEntry<'a>) -> Result<(), Error> {
        let fail = |kind| Err(Error::new(self.table.signature(), at, kind));
        let reference = |field| ErrorKind::Reference { field };
        let unique = |field| ErrorKind::Inconsistent { field };
        match *entry {
            NfitEntry::SpaRange { index, .. } if index != 0 => {
                if self.any(
                    i,
                    |e| matches!(e, NfitEntry::SpaRange { index: x, .. } if *x == index),
                )? {
                    return fail(unique("SPA Range Structure Index"));
                }
            }
            NfitEntry::Interleave { index, .. } => {
                if index == 0 {
                    return fail(ErrorKind::Value {
                        field: "Interleave Structure Index",
                    });
                }
                if self.any(
                    i,
                    |e| matches!(e, NfitEntry::Interleave { index: x, .. } if *x == index),
                )? {
                    return fail(unique("Interleave Structure Index"));
                }
            }
            NfitEntry::ControlRegion { index, .. } => {
                if self.any(
                    i,
                    |e| matches!(e, NfitEntry::ControlRegion { index: x, .. } if *x == index),
                )? {
                    return fail(unique("NVDIMM Control Region Structure Index"));
                }
            }
            NfitEntry::RegionMapping {
                device_handle,
                region_id,
                spa_index,
                control_index,
                interleave_index,
                interleave_ways,
                ..
            } => {
                if spa_index != 0
                    && !self.any(
                        i,
                        |e| matches!(e, NfitEntry::SpaRange { index: x, .. } if *x == spa_index),
                    )?
                {
                    return fail(reference("SPA Range Structure Index"));
                }
                if !self.any(i, |e| matches!(e, NfitEntry::ControlRegion { index: x, .. } if *x == control_index))? {
                    return fail(reference("NVDIMM Control Region Structure Index"));
                }
                if interleave_index != 0 {
                    if interleave_ways < 2 {
                        return fail(ErrorKind::Value {
                            field: "Interleave Ways",
                        });
                    }
                    if !self.any(i, |e| matches!(e, NfitEntry::Interleave { index: x, .. } if *x == interleave_index))? {
                        return fail(reference("Interleave Structure Index"));
                    }
                }
                if self.any(i, |e| {
                    matches!(e, NfitEntry::RegionMapping { device_handle: h, region_id: r, .. }
                        if *h == device_handle && *r == region_id)
                })? {
                    return fail(unique("NVDIMM Region ID"));
                }
            }
            NfitEntry::BlockDataWindow {
                control_index,
                windows,
                ..
            } => {
                if control_index == 0 {
                    return fail(ErrorKind::Value {
                        field: "NVDIMM Control Region Structure Index",
                    });
                }
                if !self.any(i, |e| {
                    matches!(e, NfitEntry::ControlRegion { index: x, windows: w, .. }
                        if *x == control_index && *w == windows)
                })? {
                    return fail(reference("NVDIMM Control Region Structure Index"));
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(ENTRIES)?, Shape::WORD_WORD) {
            let raw = raw?;
            tally.structure(raw.kind, raw.at, raw.bytes);
        }
        Ok(())
    }
}

fn decode<'a>(raw: &Raw<'a>, signature: Signature) -> Result<NfitEntry<'a>, Error> {
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 56)?;
            let flags = b.u16(6)?;
            let base = b.u64(32)?;
            let length = b.u64(40)?;
            if base.checked_add(length).is_none() {
                return Err(b.error(
                    40,
                    ErrorKind::Value {
                        field: "System Physical Address Range Length",
                    },
                ));
            }
            NfitEntry::SpaRange {
                index: b.u16(4)?,
                flags,
                proximity_domain: b.u32(12)?,
                range_type: b.guid(16)?,
                base,
                length,
                attributes: b.u64(48)?,
                cookie: if flags & 4 != 0 { b.opt_u64(56) } else { None },
            }
        }
        1 => {
            let b = body(raw, signature, 48)?;
            NfitEntry::RegionMapping {
                device_handle: b.u32(4)?,
                physical_id: b.u16(8)?,
                region_id: b.u16(10)?,
                spa_index: b.u16(12)?,
                control_index: b.u16(14)?,
                size: b.u64(16)?,
                offset: b.u64(24)?,
                dpa: b.u64(32)?,
                interleave_index: b.u16(40)?,
                interleave_ways: b.u16(42)?,
                state: b.u16(44)?,
            }
        }
        2 => {
            let b = body(raw, signature, 16)?;
            let count = usize::try_from(b.u32(8)?).unwrap_or(usize::MAX);
            NfitEntry::Interleave {
                index: b.u16(4)?,
                line_size: b.u32(12)?,
                lines: b
                    .array_of(16, count, 4, "Number of Lines Described")?
                    .bytes(),
            }
        }
        3 => NfitEntry::Smbios(body(raw, signature, 8)?.rest(8)?.bytes()),
        4 => {
            let b = body(raw, signature, 32)?;
            let windows = b.u16(30)?;
            if windows != 0 {
                // The window fields are in the 80-byte form only.
                body(raw, signature, 80)?;
            }
            NfitEntry::ControlRegion {
                index: b.u16(4)?,
                vendor_id: b.u16(6)?,
                device_id: b.u16(8)?,
                serial: b.u32(24)?,
                format_interface: b.u16(28)?,
                windows,
            }
        }
        5 => {
            let b = body(raw, signature, 40)?;
            NfitEntry::BlockDataWindow {
                control_index: b.u16(4)?,
                windows: b.u16(6)?,
                offset: b.u64(8)?,
                size: b.u64(16)?,
                capacity: b.u64(24)?,
            }
        }
        6 => {
            let b = body(raw, signature, 16)?;
            let count = usize::from(b.u16(8)?);
            NfitEntry::FlushHint {
                device_handle: b.u32(4)?,
                addresses: b
                    .array_of(16, count, 8, "Number of Flush Hint Addresses")?
                    .bytes(),
            }
        }
        7 => {
            let b = body(raw, signature, 16)?;
            let highest_valid = b.u8(4)?;
            let mask = 1u32
                .checked_shl(u32::from(highest_valid).saturating_add(1))
                .map_or(u32::MAX, |m| m.wrapping_sub(1));
            let capabilities = b.u32(8)? & mask;
            // A cache flush on power loss reaches the NVDIMM only through the memory controller's.
            // The rule binds only where the platform defines both bits.
            if highest_valid >= 1 && capabilities & 1 != 0 && capabilities & 2 == 0 {
                return Err(b.error(
                    8,
                    ErrorKind::Inconsistent {
                        field: "Capabilities",
                    },
                ));
            }
            NfitEntry::PlatformCapabilities {
                highest_valid,
                capabilities,
            }
        }
        _ => NfitEntry::Reserved(*raw),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    fn s(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut e = kind.to_le_bytes().to_vec();
        e.extend_from_slice(&u16::try_from(body.len() + 4).unwrap().to_le_bytes());
        e.extend_from_slice(body);
        e
    }

    pub(crate) fn spa(index: u16, base: u64, length: u64) -> Vec<u8> {
        let mut b = vec![0u8; 60];
        b[0..2].copy_from_slice(&index.to_le_bytes());
        b[2..4].copy_from_slice(&6u16.to_le_bytes());
        b[12..28].copy_from_slice(&range_type::PERSISTENT_MEMORY.0);
        b[28..36].copy_from_slice(&base.to_le_bytes());
        b[36..44].copy_from_slice(&length.to_le_bytes());
        b[44..52].copy_from_slice(&8u64.to_le_bytes());
        b[52..60].copy_from_slice(&0xC00Cu64.to_le_bytes());
        s(0, &b)
    }

    pub(crate) fn mapping(
        handle: u32,
        region: u16,
        spa: u16,
        control: u16,
        il: u16,
        ways: u16,
    ) -> Vec<u8> {
        let mut b = vec![0u8; 44];
        b[0..4].copy_from_slice(&handle.to_le_bytes());
        b[6..8].copy_from_slice(&region.to_le_bytes());
        b[8..10].copy_from_slice(&spa.to_le_bytes());
        b[10..12].copy_from_slice(&control.to_le_bytes());
        b[12..20].copy_from_slice(&0x2000_0000u64.to_le_bytes());
        b[36..38].copy_from_slice(&il.to_le_bytes());
        b[38..40].copy_from_slice(&ways.to_le_bytes());
        s(1, &b)
    }

    pub(crate) fn control(index: u16, windows: u16, long: bool) -> Vec<u8> {
        let mut b = vec![0u8; if long { 76 } else { 28 }];
        b[0..2].copy_from_slice(&index.to_le_bytes());
        b[2..4].copy_from_slice(&0x8086u16.to_le_bytes());
        b[26..28].copy_from_slice(&windows.to_le_bytes());
        s(4, &b)
    }

    fn interleave(index: u16, lines: u32) -> Vec<u8> {
        let mut b = index.to_le_bytes().to_vec();
        b.extend_from_slice(&[0, 0]);
        b.extend_from_slice(&lines.to_le_bytes());
        b.extend_from_slice(&256u32.to_le_bytes());
        for l in 0..lines {
            b.extend_from_slice(&l.to_le_bytes());
        }
        s(2, &b)
    }

    fn window(control: u16, windows: u16) -> Vec<u8> {
        let mut b = control.to_le_bytes().to_vec();
        b.extend_from_slice(&windows.to_le_bytes());
        b.extend_from_slice(&[0; 32]);
        s(5, &b)
    }

    pub(crate) fn nfit(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut body = vec![0u8; 4];
        for e in entries {
            body.extend_from_slice(e);
        }
        table(b"NFIT", 1, &body)
    }

    fn capabilities(highest: u8, bits: u32) -> Vec<u8> {
        let mut b = vec![highest, 0, 0, 0];
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        s(7, &b)
    }

    #[test]
    fn decodes_and_links_every_structure() {
        let mut flush = 0x1234u32.to_le_bytes().to_vec();
        flush.extend_from_slice(&1u16.to_le_bytes());
        flush.extend_from_slice(&[0; 6]);
        flush.extend_from_slice(&0xFEE0_0000u64.to_le_bytes());
        let t = nfit(&[
            spa(1, 0x1_0000_0000, 0x4000_0000),
            control(1, 0, false),
            control(2, 4, true),
            interleave(1, 2),
            mapping(0x1234, 0, 1, 1, 1, 2),
            mapping(0x1235, 0, 0, 2, 0, 0),
            window(2, 4),
            s(3, &[0, 0, 0, 0, 17, 40]),
            s(6, &flush),
            capabilities(1, 0b111),
            s(0x99, &[]),
        ]);
        let n = Nfit::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        let all: Vec<_> = n.entries().map(|e| e.unwrap().1).collect();
        assert!(
            matches!(all[0], NfitEntry::SpaRange { index: 1, cookie: Some(0xC00C), range_type, .. } if range_type == range_type::PERSISTENT_MEMORY)
        );
        assert!(matches!(all[3], NfitEntry::Interleave { lines, .. } if lines.len() == 8));
        assert!(matches!(all[8], NfitEntry::FlushHint { addresses, .. } if addresses.len() == 8));
        assert_eq!(
            all[9],
            NfitEntry::PlatformCapabilities {
                highest_valid: 1,
                capabilities: 0b11
            }
        );
        assert!(matches!(
            all[10],
            NfitEntry::Reserved(Raw { kind: 0x99, .. })
        ));
        let c = census::check(&t).unwrap();
        assert!(matches!(c.class, census::Class::Checked(s) if s.structures() == 11));
    }

    fn refusal(entries: &[Vec<u8>]) -> ErrorKind {
        Nfit::parse(&nfit(entries)).unwrap_err().kind
    }

    #[test]
    fn references_and_rules_are_checked() {
        let base = || vec![spa(1, 0, 0x1000), control(1, 0, false)];
        let with = |extra: Vec<Vec<u8>>| [base(), extra].concat();
        assert_eq!(
            refusal(&with(vec![mapping(1, 0, 2, 1, 0, 1)])),
            ErrorKind::Reference {
                field: "SPA Range Structure Index"
            }
        );
        assert_eq!(
            refusal(&with(vec![mapping(1, 0, 1, 9, 0, 1)])),
            ErrorKind::Reference {
                field: "NVDIMM Control Region Structure Index"
            }
        );
        assert_eq!(
            refusal(&with(vec![mapping(1, 0, 1, 1, 3, 2)])),
            ErrorKind::Reference {
                field: "Interleave Structure Index"
            }
        );
        assert_eq!(
            refusal(&with(vec![interleave(3, 1), mapping(1, 0, 1, 1, 3, 1)])),
            ErrorKind::Value {
                field: "Interleave Ways"
            }
        );
        assert_eq!(
            refusal(&with(vec![
                mapping(1, 0, 1, 1, 0, 1),
                mapping(1, 0, 1, 1, 0, 1)
            ])),
            ErrorKind::Inconsistent {
                field: "NVDIMM Region ID"
            }
        );
        assert!(
            Nfit::parse(&nfit(&with(vec![
                mapping(1, 0, 1, 1, 0, 1),
                mapping(2, 0, 1, 1, 0, 1)
            ])))
            .is_ok()
        );
        assert_eq!(
            refusal(&with(vec![spa(1, 0, 1)])),
            ErrorKind::Inconsistent {
                field: "SPA Range Structure Index"
            }
        );
        assert_eq!(
            refusal(&[interleave(0, 1)]),
            ErrorKind::Value {
                field: "Interleave Structure Index"
            }
        );
        assert!(matches!(
            refusal(&[control(1, 2, false)]),
            ErrorKind::Structure { .. }
        ));
        assert_eq!(
            refusal(&[control(1, 2, true), window(1, 3)]),
            ErrorKind::Reference {
                field: "NVDIMM Control Region Structure Index"
            }
        );
        assert_eq!(
            refusal(&[capabilities(2, 0b001)]),
            ErrorKind::Inconsistent {
                field: "Capabilities"
            }
        );
        assert!(
            Nfit::parse(&nfit(&[capabilities(0, 0b001)])).is_ok(),
            "bit 0 is the only valid bit"
        );
        assert!(matches!(
            refusal(&[spa(1, u64::MAX, 2)]),
            ErrorKind::Value { .. }
        ));
        let mut lines = interleave(1, 4);
        lines.truncate(lines.len() - 4);
        lines[2] = u8::try_from(lines.len()).unwrap();
        assert!(matches!(refusal(&[lines]), ErrorKind::Offset { .. }));
        let many = vec![s(0x99, &[]); MAX_STRUCTURES + 1];
        assert!(Nfit::parse(&nfit(&many[..MAX_STRUCTURES])).is_ok());
        assert!(matches!(refusal(&many), ErrorKind::Value { .. }));
    }
}
