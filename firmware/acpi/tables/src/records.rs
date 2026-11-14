//! The firmware's records and pointers of ACPI 6.6 §5.2.30 and §5.2.32 to §5.2.36: the Platform
//! Debug Trigger Table (PDTT), the Platform Health Assessment Table (PHAT), the Miscellaneous
//! GUIDed Table Entries (MISC), the CC Event Log table (CCEL) and the Storage Volume Key Location
//! table (SVKL).
//!
//! §5.2.35 and §5.2.36 give the CCEL's and the SVKL's layouts, and Table 5.5 of §5.2.6 sends their
//! full definitions to documents outside ACPI 6.6 (the confidential computing event log's and Intel
//! TDX's); what those documents add is not supported here. §5.2.36 prints the SVKL's signature as
//! `SKVL` once; Table 5.5's `SVKL` is the one read.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::{Bytes, Guid};
use crate::walk::{Raw, Shape, Walk, Width, body};

/// One debug trigger (§5.2.30, Table 5.183).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    /// The PCC subspace to ring.
    pub subspace: u8,
    /// It may be rung while the system runs, not only after a fatal crash.
    pub runtime: bool,
    /// The next trigger waits for this one to complete.
    pub wait: bool,
    /// Rung after crash-dump processing, not before.
    pub after_dump: bool,
}

/// The checked PDTT.
#[derive(Clone, Copy, Debug)]
pub struct Pdtt<'a> {
    triggers: Bytes<'a>,
}

impl<'a> Pdtt<'a> {
    /// Checks the table. Refused: no trigger (§5.2.30 requires one), or an array offset inside the
    /// fixed fields or an array that runs past the end.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"PDTT")?.reader();
        b.require(44)?;
        let count = usize::from(b.u8(36)?);
        if count == 0 {
            return Err(b.error(
                36,
                ErrorKind::Value {
                    field: "Trigger Count",
                },
            ));
        }
        let offset = usize::try_from(b.u32(40)?).unwrap_or(usize::MAX);
        if offset < 44 {
            return Err(b.error(
                40,
                ErrorKind::Offset {
                    field: "Trigger Identifier Array Offset",
                },
            ));
        }
        Ok(Self {
            triggers: b.array_of(offset, count, 2, ("Trigger Identifier Array Offset", 40))?,
        })
    }

    /// The triggers, in the order they are rung.
    pub fn triggers(&self) -> impl Iterator<Item = Trigger> + 'a {
        self.triggers
            .bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[subspace, flags]| Trigger {
                subspace,
                runtime: flags & 1 != 0,
                wait: flags & 2 != 0,
                after_dump: flags & 4 != 0,
            })
    }
}

/// The GUID of the PHAT's reset reason record (§5.2.32.5).
pub const RESET_REASON: Guid = Guid::from_fields(
    0x7A01_4CE2,
    0xF263,
    0x4B77,
    [0xB8, 0x8A, 0xE6, 0x33, 0x6B, 0x78, 0x2C, 0x14],
);

/// The last reset, as the PHAT's reset reason record gives it (§5.2.32.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetReason<'a> {
    /// The sources the platform can report, one bit each: unknown, hardware, firmware, software,
    /// supervisor.
    pub supported_sources: u8,
    /// This reset's source, in the same bits.
    pub source: u8,
    /// Its sub-source (for software: 1 the OS, 2 a hypervisor).
    pub sub_source: u8,
    /// Its reason: 0 unknown; 1 to 4 expected; 32 to 37 unexpected.
    pub reason: u8,
    /// The vendors' entries: a GUID, a 16-bit length counting the entry's 20-byte header, a
    /// revision, and data.
    pub vendor: &'a [u8],
}

/// One PHAT record (§5.2.32.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhatRecord<'a> {
    /// Firmware versions (type 0): 28-byte elements, each a component's GUID, a 64-bit version and
    /// a producer's ID.
    Versions(&'a [u8]),
    /// A firmware health record (type 1).
    Health {
        /// 0 errors, 1 none, 2 unknown, 3 advisory.
        am_healthy: u8,
        /// The device, or what kind of record this is.
        signature: Guid,
        /// The producer's device path, kept as bytes (§5.2.32.5 gives a size that fits neither of
        /// its forms with certainty).
        device_path: &'a [u8],
        /// The device's own data.
        data: &'a [u8],
        /// The reset reason, when the signature names that record.
        reset: Option<ResetReason<'a>>,
    },
    /// A type ACPI 6.6 reserves or leaves to vendors.
    Other(Raw<'a>),
}

/// A PHAT record's shape: a 16-bit type, then a 16-bit length.
const RECORD: Shape = Shape::WORD_WORD;

fn record<'a>(raw: &Raw<'a>, signature: Signature) -> Result<PhatRecord<'a>, Error> {
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 12)?;
            let count = usize::try_from(b.u32(8)?).unwrap_or(usize::MAX);
            PhatRecord::Versions(b.array_of(12, count, 28, ("Record Count", 8))?.bytes())
        }
        1 => {
            let b = body(raw, signature, 28)?;
            let guid = b.guid(8)?;
            let offset = usize::try_from(b.u32(24)?).unwrap_or(usize::MAX);
            let (device_path, data) = if offset == 0 {
                (b.rest(28)?.bytes(), &[][..])
            } else {
                if offset < 28 || offset > b.len() {
                    return Err(b.error(
                        24,
                        ErrorKind::Offset {
                            field: "Device-specific Data Offset",
                        },
                    ));
                }
                (
                    b.get(28, offset.saturating_sub(28))?,
                    b.rest(offset)?.bytes(),
                )
            };
            let reset = if guid == RESET_REASON && offset != 0 {
                Some(reset_reason(&b.rest(offset)?)?)
            } else {
                None
            };
            PhatRecord::Health {
                am_healthy: b.u8(7)?,
                signature: guid,
                device_path,
                data,
                reset,
            }
        }
        _ => PhatRecord::Other(*raw),
    })
}

/// The reset reason structure; its vendors' entries must fit it, each at least its 20-byte header.
fn reset_reason<'a>(b: &Bytes<'a>) -> Result<ResetReason<'a>, Error> {
    b.require(6)?;
    let vendor = b.rest(6)?;
    let entries = Shape {
        kind_at: 0,
        kind: Width::U16,
        len_at: 16,
        len: Width::U16,
    };
    let mut count = 0u16;
    for raw in Walk::new(vendor, entries) {
        let raw = raw?;
        if raw.bytes.len() < 20 {
            return Err(vendor.error(
                raw.at.saturating_sub(vendor.base()),
                ErrorKind::Structure {
                    kind: raw.kind,
                    length: raw.bytes.len(),
                },
            ));
        }
        count = count.saturating_add(1);
    }
    if count != b.u16(4)? {
        return Err(b.error(
            4,
            ErrorKind::Offset {
                field: "Vendor Count",
            },
        ));
    }
    Ok(ResetReason {
        supported_sources: b.u8(0)?,
        source: b.u8(1)?,
        sub_source: b.u8(2)?,
        reason: b.u8(3)?,
        vendor: vendor.bytes(),
    })
}

/// The checked PHAT.
#[derive(Clone, Copy, Debug)]
pub struct Phat<'a> {
    table: Table<'a>,
}

impl<'a> Phat<'a> {
    /// Checks the table: every record fits, a version record holds its elements, a health record's
    /// data offset is past its fixed fields and inside it, and a reset reason record's vendor
    /// entries are whole and as many as it counts.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"PHAT")?;
        let phat = Self { table };
        for r in phat.records() {
            r?;
        }
        Ok(phat)
    }

    /// The records, in order.
    pub fn records(&self) -> impl Iterator<Item = Result<PhatRecord<'a>, Error>> + 'a {
        let signature = self.table.signature();
        self.table
            .reader()
            .rest(header::HEADER_LEN)
            .ok()
            .into_iter()
            .flat_map(|run| Walk::new(run, RECORD))
            .map(move |raw| record(&raw?, signature))
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(header::HEADER_LEN)?, RECORD) {
            let raw = raw?;
            tally.structure(raw.kind, raw.at, raw.bytes);
        }
        Ok(())
    }
}

/// One GUIDed entry of the MISC table (§5.2.34).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MiscEntry<'a> {
    /// The entry's format.
    pub guid: Guid,
    /// The format's revision.
    pub revision: u32,
    /// The producer's ACPI vendor ID.
    pub producer: u32,
    /// The data.
    pub data: &'a [u8],
}

/// A GUIDed entry's shape: the length is the 32-bit field after the 16-byte GUID; the "type" read
/// here is the GUID's first two bytes, which only the census counts.
const MISC_ENTRY: Shape = Shape {
    kind_at: 0,
    kind: Width::U16,
    len_at: 16,
    len: Width::U32,
};

/// The checked MISC table.
#[derive(Clone, Copy, Debug)]
pub struct Misc<'a> {
    table: Table<'a>,
}

impl<'a> Misc<'a> {
    /// Checks the table: at least one entry (§5.2.34), each of at least its 28-byte header and
    /// inside the table.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"MISC")?;
        let misc = Self { table };
        let mut any = false;
        for e in misc.entries() {
            e?;
            any = true;
        }
        if !any {
            return Err(Error::new(
                table.signature(),
                header::HEADER_LEN,
                ErrorKind::Value {
                    field: "GUIDed Entries",
                },
            ));
        }
        Ok(misc)
    }

    /// The entries, in order.
    pub fn entries(&self) -> impl Iterator<Item = Result<MiscEntry<'a>, Error>> + 'a {
        let signature = self.table.signature();
        self.table
            .reader()
            .rest(header::HEADER_LEN)
            .ok()
            .into_iter()
            .flat_map(|run| Walk::new(run, MISC_ENTRY))
            .map(move |raw| {
                let b = body(&raw?, signature, 28)?;
                Ok(MiscEntry {
                    guid: b.guid(0)?,
                    revision: b.u32(20)?,
                    producer: b.u32(24)?,
                    data: b.rest(28)?.bytes(),
                })
            })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(header::HEADER_LEN)?, MISC_ENTRY) {
            let raw = raw?;
            body(&raw, self.table.signature(), 28)?;
            tally.structure(0, raw.at, raw.bytes);
        }
        Ok(())
    }
}

/// The confidential computing technology a CCEL is for (§5.2.35).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CcType {
    /// AMD SEV (1).
    AmdSev,
    /// Intel TDX (2).
    IntelTdx,
    /// A value ACPI 6.6 reserves.
    Reserved(u8),
}

/// The CC Event Log table, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ccel {
    /// The technology.
    pub cc_type: CcType,
    /// Its subtype, which §5.2.35 does not define.
    pub subtype: u8,
    /// The log area's minimum length.
    pub log_length: u64,
    /// The log area's physical address.
    pub log_address: u64,
}

impl Ccel {
    /// Checks the table. Refused: a log area that wraps past the top of the address space.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"CCEL")?.reader();
        b.require(56)?;
        let log_length = b.u64(40)?;
        let log_address = b.u64(48)?;
        if log_address.checked_add(log_length).is_none() {
            return Err(b.error(
                40,
                ErrorKind::Value {
                    field: "Log Area Minimum Length",
                },
            ));
        }
        Ok(Self {
            cc_type: match b.u8(36)? {
                1 => CcType::AmdSev,
                2 => CcType::IntelTdx,
                other => CcType::Reserved(other),
            },
            subtype: b.u8(37)?,
            log_length,
            log_address,
        })
    }
}

/// One key the SVKL locates (§5.2.36).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeKey {
    /// 0: the main storage volume key; others reserved.
    pub key_type: u16,
    /// 0: raw binary; others reserved.
    pub format: u16,
    /// Its size in bytes.
    pub size: u32,
    /// Its guest-physical address.
    pub address: u64,
}

/// The checked SVKL.
#[derive(Clone, Copy, Debug)]
pub struct Svkl<'a> {
    keys: Bytes<'a>,
}

impl<'a> Svkl<'a> {
    /// Checks the table. Refused: keys that do not fit, and a key whose bytes wrap past the top of
    /// the address space.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"SVKL")?.reader();
        b.require(40)?;
        let count = usize::try_from(b.u32(36)?).unwrap_or(usize::MAX);
        let svkl = Self {
            keys: b.array_of(40, count, 16, ("Key Count", 36))?,
        };
        for (i, key) in svkl.keys().enumerate() {
            if key.address.checked_add(u64::from(key.size)).is_none() {
                return Err(b.error(
                    i.saturating_mul(16).saturating_add(48),
                    ErrorKind::Value {
                        field: "Key Address",
                    },
                ));
            }
        }
        Ok(svkl)
    }

    /// The keys.
    pub fn keys(&self) -> impl Iterator<Item = VolumeKey> + 'a {
        self.keys.bytes().as_chunks::<16>().0.iter().map(|k| {
            let [t0, t1, f0, f1, s0, s1, s2, s3, a @ ..] = *k;
            VolumeKey {
                key_type: u16::from_le_bytes([t0, t1]),
                format: u16::from_le_bytes([f0, f1]),
                size: u32::from_le_bytes([s0, s1, s2, s3]),
                address: u64::from_le_bytes(a),
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        let mut pdtt = vec![2, 0, 0, 0];
        pdtt.extend_from_slice(&44u32.to_le_bytes());
        pdtt.extend_from_slice(&[0x01, 0x02, 0x03, 0x05]);
        let mut versions = vec![0, 0, 40, 0, 1, 0, 0, 0, 1, 0, 0, 0];
        versions.extend_from_slice(&[0x11; 28]);
        let mut reason = vec![0b1111, 0b1000, 1, 3, 1, 0];
        reason.extend_from_slice(&[0x22; 16]);
        reason.extend_from_slice(&24u16.to_le_bytes());
        reason.extend_from_slice(&[1, 0, 0xAA, 0xBB, 0xCC, 0xDD]);
        let mut misc = vec![0x33; 16];
        misc.extend_from_slice(&30u32.to_le_bytes());
        misc.extend_from_slice(&1u32.to_le_bytes());
        misc.extend_from_slice(b"ABCD");
        misc.extend_from_slice(&[7, 8]);
        let mut ccel = vec![2, 0, 0, 0];
        ccel.extend_from_slice(&0x1_0000u64.to_le_bytes());
        ccel.extend_from_slice(&0x7F00_0000u64.to_le_bytes());
        let mut svkl = 1u32.to_le_bytes().to_vec();
        svkl.extend_from_slice(&[0, 0, 0, 0]);
        svkl.extend_from_slice(&32u32.to_le_bytes());
        svkl.extend_from_slice(&0x7E00_0000u64.to_le_bytes());
        vec![
            table(b"PDTT", 0, &pdtt),
            table(
                b"PHAT",
                1,
                &[versions, health(RESET_REASON, &[0x7F; 88], &reason)].concat(),
            ),
            table(b"MISC", 1, &misc),
            table(b"CCEL", 1, &ccel),
            table(b"SVKL", 1, &svkl),
        ]
    }

    #[test]
    fn pdtt_lists_its_triggers() {
        let mut body = vec![2, 0, 0, 0];
        body.extend_from_slice(&44u32.to_le_bytes());
        body.extend_from_slice(&[0x01, 0x02, 0x03, 0x05]);
        let t = table(b"PDTT", 0, &body);
        let p = Pdtt::parse(&t).unwrap();
        let t: Vec<_> = p.triggers().collect();
        assert_eq!(
            t[0],
            Trigger {
                subspace: 1,
                runtime: false,
                wait: true,
                after_dump: false
            }
        );
        assert_eq!(
            t[1],
            Trigger {
                subspace: 3,
                runtime: true,
                wait: false,
                after_dump: true
            }
        );
        let kind = |b: &[u8]| Pdtt::parse(&table(b"PDTT", 0, b)).unwrap_err().kind;
        let mut none = body.clone();
        none[0] = 0;
        assert_eq!(
            kind(&none),
            ErrorKind::Value {
                field: "Trigger Count"
            }
        );
        let mut far = body.clone();
        far[0] = 3;
        assert!(matches!(kind(&far), ErrorKind::Offset { .. }));
        let mut inside = body.clone();
        inside[4] = 40;
        assert!(matches!(kind(&inside), ErrorKind::Offset { .. }));
    }

    fn health(guid: Guid, path: &[u8], data: &[u8]) -> Vec<u8> {
        let len = 28 + path.len() + data.len();
        let mut r = 1u16.to_le_bytes().to_vec();
        r.extend_from_slice(&u16::try_from(len).unwrap().to_le_bytes());
        r.extend_from_slice(&[1, 0, 0, 1]);
        r.extend_from_slice(&guid.0);
        let offset = if data.is_empty() { 0 } else { 28 + path.len() };
        r.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
        r.extend_from_slice(path);
        r.extend_from_slice(data);
        r
    }

    #[test]
    fn phat_decodes_versions_health_and_reset_reason() {
        let mut versions = vec![0, 0, 40, 0, 1, 0, 0, 0, 1, 0, 0, 0];
        versions.extend_from_slice(&[0x11; 28]);
        let mut reason = vec![0b1111, 0b1000, 1, 3, 1, 0];
        reason.extend_from_slice(&[0x22; 16]);
        reason.extend_from_slice(&24u16.to_le_bytes());
        reason.extend_from_slice(&[1, 0, 0xAA, 0xBB, 0xCC, 0xDD]);
        let t = table(
            b"PHAT",
            1,
            &[
                versions,
                health(RESET_REASON, &[0x7F; 88], &reason),
                health(Guid([5; 16]), &[1, 2], &[]),
            ]
            .concat(),
        );
        let p = Phat::parse(&t).unwrap_or_else(|e| panic!("{e:?}"));
        let all: Vec<_> = p.records().map(Result::unwrap).collect();
        assert!(matches!(all[0], PhatRecord::Versions(v) if v.len() == 28));
        assert!(matches!(
            all[1],
            PhatRecord::Health { reset: Some(ResetReason { source: 0b1000, sub_source: 1, reason: 3, .. }), device_path, .. } if device_path.len() == 88
        ));
        assert!(matches!(
            all[2],
            PhatRecord::Health {
                reset: None,
                data: [],
                ..
            }
        ));
        assert!(
            matches!(census::check(&t).unwrap().class, census::Class::Checked(s) if s.structures() == 3)
        );
        let kind = |b: Vec<u8>| Phat::parse(&table(b"PHAT", 1, &b)).unwrap_err().kind;
        let mut wrong_count = reason.clone();
        wrong_count[4] = 2;
        assert!(matches!(
            kind(health(RESET_REASON, &[0; 4], &wrong_count)),
            ErrorKind::Offset {
                field: "Vendor Count"
            }
        ));
        let mut inside = health(Guid([5; 16]), &[1, 2], &[3]);
        inside[24] = 20;
        assert!(matches!(kind(inside), ErrorKind::Offset { .. }));
        let mut many = vec![0, 0, 12, 0, 1, 0, 0, 0, 1, 0, 0, 0];
        many[8] = 2;
        assert!(matches!(
            kind(many),
            ErrorKind::Offset {
                field: "Record Count"
            }
        ));
    }

    #[test]
    fn misc_ccel_and_svkl() {
        let mut entry = vec![0x33; 16];
        entry.extend_from_slice(&30u32.to_le_bytes());
        entry.extend_from_slice(&1u32.to_le_bytes());
        entry.extend_from_slice(b"ABCD");
        entry.extend_from_slice(&[7, 8]);
        let t = table(b"MISC", 1, &entry);
        let m = Misc::parse(&t).unwrap();
        assert_eq!(m.entries().next().unwrap().unwrap().data, &[7, 8]);
        assert!(matches!(
            Misc::parse(&table(b"MISC", 1, &[])).unwrap_err().kind,
            ErrorKind::Value {
                field: "GUIDed Entries"
            }
        ));
        let mut ccel = vec![2, 0, 0, 0];
        ccel.extend_from_slice(&0x1_0000u64.to_le_bytes());
        ccel.extend_from_slice(&0x7F00_0000u64.to_le_bytes());
        assert_eq!(
            Ccel::parse(&table(b"CCEL", 1, &ccel)).unwrap().cc_type,
            CcType::IntelTdx
        );
        ccel[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Ccel::parse(&table(b"CCEL", 1, &ccel)).is_err());
        let mut svkl = 1u32.to_le_bytes().to_vec();
        svkl.extend_from_slice(&[0, 0, 0, 0]);
        svkl.extend_from_slice(&32u32.to_le_bytes());
        svkl.extend_from_slice(&0x7E00_0000u64.to_le_bytes());
        let t = table(b"SVKL", 1, &svkl);
        let s = Svkl::parse(&t).unwrap();
        assert_eq!(
            s.keys().next().unwrap(),
            VolumeKey {
                key_type: 0,
                format: 0,
                size: 32,
                address: 0x7E00_0000
            }
        );
        svkl[0] = 2;
        assert!(matches!(
            Svkl::parse(&table(b"SVKL", 1, &svkl)).unwrap_err().kind,
            ErrorKind::Offset { .. }
        ));
        svkl[0] = 1;
        svkl[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Svkl::parse(&table(b"SVKL", 1, &svkl)).is_err());
    }
}
