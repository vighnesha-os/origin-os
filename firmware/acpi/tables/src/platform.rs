//! The small tables of ACPI 6.6 §5.2.14 to §5.2.19: the SBST (§5.2.14), the ECDT (§5.2.15), the
//! SLIT (§5.2.17), the CPEP (§5.2.18) and the MSCT (§5.2.19).

use crate::census::Tally;
use crate::error::{Error, ErrorKind};
use crate::gas::{AddressSpace, Gas};
use crate::header::{self, Table};
use crate::walk::{Raw, Shape, Walk, body};

/// The Smart Battery Table, signature `SBST` (§5.2.14): the energy levels, in mWh, at which the
/// OEM suggests the OS warn, sleep and shut down. §5.2.14 states no order among them, so none is
/// checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sbst {
    /// Warn the user.
    pub warning: u32,
    /// Put the machine to sleep.
    pub low: u32,
    /// Shut down.
    pub critical: u32,
}

impl Sbst {
    /// Checks the table and reads its three levels.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"SBST")?.reader();
        b.require(48)?;
        Ok(Self {
            warning: b.u32(36)?,
            low: b.u32(40)?,
            critical: b.u32(44)?,
        })
    }
}

/// The Embedded Controller Boot Resources Table, signature `ECDT` (§5.2.15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ecdt<'a> {
    /// The EC's command and status register.
    pub control: Gas,
    /// The EC's data register.
    pub data: Gas,
    /// The `_UID` of the EC's device.
    pub uid: u32,
    /// The SCI's bit in a GPE status register.
    pub gpe_bit: u8,
    /// The EC device's full namespace path, without its terminating NUL.
    pub id: &'a [u8],
}

impl<'a> Ecdt<'a> {
    /// Checks the table. Refused: a register outside system memory and system I/O, which §5.2.15
    /// allows alone; a path with no terminating NUL inside the table, an empty one, or one with a
    /// character no namespace path can hold.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"ECDT")?.reader();
        b.require(66)?;
        let mut registers = [Gas::read(&b, 36)?, Gas::read(&b, 48)?];
        for (reg, (at, name)) in registers
            .iter_mut()
            .zip([(36, "EC_CONTROL"), (48, "EC_DATA")])
        {
            reg.check_register(&b, at, name)?;
            if !matches!(
                reg.space,
                AddressSpace::SystemIo | AddressSpace::SystemMemory
            ) {
                return Err(b.error(at, ErrorKind::Value { field: name }));
            }
        }
        let [control, data] = registers;
        let tail = b.rest(65)?.bytes();
        let end = tail
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| b.error(65, ErrorKind::Value { field: "EC_ID" }))?;
        let id = tail.get(..end).unwrap_or(&[]);
        let path_char =
            |c: &u8| c.is_ascii_uppercase() || c.is_ascii_digit() || b"_.\\^".contains(c);
        if id.is_empty() || !id.iter().all(path_char) {
            return Err(b.error(65, ErrorKind::Value { field: "EC_ID" }));
        }
        Ok(Self {
            control,
            data,
            uid: b.u32(60)?,
            gpe_bit: b.u8(64)?,
            id,
        })
    }
}

/// The System Locality Information Table, signature `SLIT` (§5.2.17).
#[derive(Clone, Copy, Debug)]
pub struct Slit<'a> {
    localities: usize,
    matrix: &'a [u8],
}

impl<'a> Slit<'a> {
    /// Checks the table: the matrix fits, every locality is 10 from itself, and no distance is one
    /// of the values 0 to 9 that ACPI 6.6 reserves (255 means unreachable). Distances need not be
    /// symmetric.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let b = header::check_signature(bytes, b"SLIT")?.reader();
        b.require(44)?;
        let count = b.u64(36)?;
        let offset = || {
            b.error(
                36,
                ErrorKind::Offset {
                    field: "Number of System Localities",
                },
            )
        };
        let localities = usize::try_from(count).map_err(|_| offset())?;
        let cells = localities.checked_mul(localities).ok_or_else(offset)?;
        let matrix = b.sub(44, cells).map_err(|_| offset())?;
        for (cell, &distance) in matrix.bytes().iter().enumerate() {
            let diagonal =
                localities != 0 && cell.checked_rem(localities.saturating_add(1)) == Some(0);
            if distance < 10 || (diagonal && distance != 10) {
                return Err(matrix.error(cell, ErrorKind::Value { field: "Entry" }));
            }
        }
        Ok(Self {
            localities,
            matrix: matrix.bytes(),
        })
    }

    /// How many localities.
    #[must_use]
    pub const fn localities(&self) -> usize {
        self.localities
    }

    /// The distance from locality `i` to locality `j`.
    #[must_use]
    pub fn distance(&self, i: usize, j: usize) -> Option<u8> {
        if i >= self.localities || j >= self.localities {
            return None;
        }
        let cell = i.checked_mul(self.localities)?.checked_add(j)?;
        self.matrix.get(cell).copied()
    }
}

/// One processor the CPEP asks the OS to poll for corrected platform errors (§5.2.18.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpepEntry<'a> {
    /// Poll this processor (type 0).
    Processor {
        /// The processor's ID, a one-byte local APIC or SAPIC ID.
        id: u8,
        /// The processor's EID, which only a SAPIC has.
        eid: u8,
        /// The suggested interval in milliseconds.
        interval_ms: u32,
    },
    /// A type ACPI 6.6 does not define.
    Reserved(Raw<'a>),
}

/// The Corrected Platform Error Polling Table, signature `CPEP` (§5.2.18).
#[derive(Clone, Copy, Debug)]
pub struct Cpep<'a> {
    table: Table<'a>,
}

impl<'a> Cpep<'a> {
    /// Checks the table; its structures come from [`Cpep::entries`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"CPEP")?;
        table.reader().require(44)?;
        let cpep = Self { table };
        for entry in cpep.entries() {
            entry?;
        }
        Ok(cpep)
    }

    /// Every structure, in order.
    pub fn entries(&self) -> impl Iterator<Item = Result<CpepEntry<'a>, Error>> + 'a {
        let signature = self.table.signature();
        let walk = self.table.reader().rest(44).ok();
        walk.into_iter()
            .flat_map(|run| Walk::new(run, Shape::BYTE_BYTE))
            .map(move |raw| {
                let raw = raw?;
                if raw.kind != 0 {
                    return Ok(CpepEntry::Reserved(raw));
                }
                let b = body(&raw, signature, 8)?;
                Ok(CpepEntry::Processor {
                    id: b.u8(2)?,
                    eid: b.u8(3)?,
                    interval_ms: b.u32(4)?,
                })
            })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(44)?, Shape::BYTE_BYTE) {
            let raw = raw?;
            if raw.kind == 0 {
                body(&raw, self.table.signature(), 8)?;
            }
            tally.structure(raw.kind, raw.at, raw.bytes);
        }
        Ok(())
    }
}

/// One range of proximity domains and the most each can hold (§5.2.19.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomainRange {
    /// The first domain.
    pub low: u32,
    /// The last domain.
    pub high: u32,
    /// The most processors each domain can hold.
    pub max_processors: u32,
    /// The most memory, in bytes.
    pub max_memory: u64,
}

/// The Maximum System Characteristics Table, signature `MSCT` (§5.2.19).
#[derive(Clone, Copy, Debug)]
pub struct Msct<'a> {
    table: Table<'a>,
    offset: usize,
    /// The highest proximity domain the system can ever have (the table holds the count less one).
    pub max_proximity_domain: u32,
    /// The highest clock domain the system can ever have.
    pub max_clock_domain: u32,
    /// The highest physical address the system can ever reach.
    pub max_physical_address: u64,
}

impl<'a> Msct<'a> {
    /// Checks the table. Refused: an offset to the ranges that falls in the fixed fields or past
    /// the end; a range whose last domain is below its first; ranges out of ascending order or
    /// overlapping; and a domain up to the maximum that no range covers, which §5.2.19 forbids.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"MSCT")?;
        let b = table.reader();
        b.require(56)?;
        let offset = usize::try_from(b.u32(36)?).unwrap_or(usize::MAX);
        if offset < 56 || offset > b.len() {
            return Err(b.error(
                36,
                ErrorKind::Offset {
                    field: "OffsetProxDomInfo",
                },
            ));
        }
        let msct = Self {
            table,
            offset,
            max_proximity_domain: b.u32(40)?,
            max_clock_domain: b.u32(44)?,
            max_physical_address: b.u64(48)?,
        };
        let mut next = Some(0u32);
        for range in msct.ranges() {
            let (at, range) = range?;
            let refuse = |field| Err(b.error(at, ErrorKind::Value { field }));
            if range.high < range.low {
                return refuse("Proximity Domain Range (high)");
            }
            match next {
                Some(n) if range.low == n => {}
                Some(n) if range.low > n && n <= msct.max_proximity_domain => {
                    return refuse("a proximity domain no range covers");
                }
                // A gap above the maximum leaves no domain the system can have uncovered.
                Some(n) if range.low > n => {}
                Some(_) => return refuse("Proximity Domain Range (low)"),
                None => return refuse("Proximity Domain Range (low)"),
            }
            next = range.high.checked_add(1);
        }
        if next.is_some_and(|n| n <= msct.max_proximity_domain) {
            return Err(b.error(
                b.len(),
                ErrorKind::Value {
                    field: "a proximity domain no range covers",
                },
            ));
        }
        Ok(msct)
    }

    /// The ranges, each with its offset in the table.
    pub fn ranges(&self) -> impl Iterator<Item = Result<(usize, DomainRange), Error>> + 'a {
        let signature = self.table.signature();
        let walk = self.table.reader().rest(self.offset).ok();
        walk.into_iter()
            .flat_map(|run| Walk::new(run, Shape::BYTE_BYTE))
            .map(move |raw| {
                let raw = raw?;
                let b = body(&raw, signature, 22)?;
                Ok((
                    raw.at,
                    DomainRange {
                        low: b.u32(2)?,
                        high: b.u32(6)?,
                        max_processors: b.u32(10)?,
                        max_memory: b.u64(14)?,
                    },
                ))
            })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(self.offset)?, Shape::BYTE_BYTE) {
            let raw = raw?;
            // The first byte is the structure's revision, not a type: counted as one kind.
            tally.structure(0, raw.at, raw.bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{gas, table};

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        let mut sbst = Vec::new();
        for v in [5000u32, 2000, 500] {
            sbst.extend_from_slice(&v.to_le_bytes());
        }
        let mut cpep = vec![0u8; 8];
        cpep.extend_from_slice(&[0, 8, 3, 0, 100, 0, 0, 0]);
        vec![
            table(b"SBST", 1, &sbst),
            ecdt(gas(1, 8, 0, 1, 0x66), b"\\_SB.PCI0.LPCB.EC0\0"),
            slit(2, &[10, 21, 21, 10]),
            table(b"CPEP", 1, &cpep),
            msct(56, 3, &[(0, 1), (2, 3)]),
        ]
    }

    #[test]
    fn sbst_reads_its_levels() {
        let mut body = Vec::new();
        for v in [5000u32, 2000, 500] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        let s = Sbst::parse(&table(b"SBST", 1, &body)).unwrap();
        assert_eq!((s.warning, s.low, s.critical), (5000, 2000, 500));
        assert!(Sbst::parse(&table(b"SBST", 1, &body[..11])).is_err());
    }

    fn ecdt(control: [u8; 12], id: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&control);
        body.extend_from_slice(&gas(1, 8, 0, 1, 0x62));
        body.extend_from_slice(&1u32.to_le_bytes());
        body.push(0x16);
        body.extend_from_slice(id);
        table(b"ECDT", 1, &body)
    }

    #[test]
    fn ecdt_reads_its_registers_and_path() {
        let t = ecdt(gas(1, 8, 0, 1, 0x66), b"\\_SB.PCI0.LPCB.EC0\0");
        let e = Ecdt::parse(&t).unwrap();
        assert_eq!(
            (e.control.address, e.data.address, e.gpe_bit),
            (0x66, 0x62, 0x16)
        );
        assert_eq!(e.id, b"\\_SB.PCI0.LPCB.EC0");
        let refuse = |t: Vec<u8>| Ecdt::parse(&t).unwrap_err();
        assert_eq!(
            refuse(ecdt(gas(3, 8, 0, 1, 0x66), b"\\EC\0")).kind,
            ErrorKind::Value {
                field: "EC_CONTROL"
            }
        );
        assert_eq!(
            refuse(ecdt(gas(1, 8, 0, 1, 0x66), b"\\EC")).kind,
            ErrorKind::Value { field: "EC_ID" }
        );
        assert_eq!(
            refuse(ecdt(gas(1, 8, 0, 1, 0x66), b"\0")).kind,
            ErrorKind::Value { field: "EC_ID" }
        );
        assert_eq!(
            refuse(ecdt(gas(1, 8, 0, 1, 0x66), b"\\e\n\0")).kind,
            ErrorKind::Value { field: "EC_ID" }
        );
    }

    fn slit(n: u64, cells: &[u8]) -> Vec<u8> {
        let mut body = n.to_le_bytes().to_vec();
        body.extend_from_slice(cells);
        table(b"SLIT", 1, &body)
    }

    #[test]
    fn slit_checks_its_matrix() {
        let t = slit(2, &[10, 21, 255, 10]);
        let s = Slit::parse(&t).unwrap();
        assert_eq!(
            (
                s.localities(),
                s.distance(0, 1),
                s.distance(1, 0),
                s.distance(2, 0)
            ),
            (2, Some(21), Some(255), None)
        );
        let e = Slit::parse(&slit(2, &[10, 21, 21, 11])).unwrap_err();
        assert_eq!((e.at, e.kind), (47, ErrorKind::Value { field: "Entry" }));
        assert!(Slit::parse(&slit(2, &[10, 9, 21, 10])).is_err());
        for n in [3, u64::MAX, 1 << 32] {
            assert!(
                matches!(
                    Slit::parse(&slit(n, &[10; 4])).unwrap_err().kind,
                    ErrorKind::Offset { .. }
                ),
                "{n}"
            );
        }
        assert_eq!(Slit::parse(&slit(0, &[])).unwrap().localities(), 0);
    }

    #[test]
    fn cpep_lists_processors() {
        let mut body = vec![0u8; 8];
        body.extend_from_slice(&[0, 8, 3, 0, 100, 0, 0, 0]);
        body.extend_from_slice(&[7, 2]);
        let t = table(b"CPEP", 1, &body);
        let c = Cpep::parse(&t).unwrap();
        let all: Vec<_> = c.entries().map(Result::unwrap).collect();
        assert_eq!(
            all[0],
            CpepEntry::Processor {
                id: 3,
                eid: 0,
                interval_ms: 100
            }
        );
        assert!(matches!(all[1], CpepEntry::Reserved(Raw { kind: 7, .. })));
        let mut short = vec![0u8; 8];
        short.extend_from_slice(&[0, 7, 0, 0, 0, 0, 0]);
        assert!(Cpep::parse(&table(b"CPEP", 1, &short)).is_err());
    }

    fn msct(offset: u32, max: u32, ranges: &[(u32, u32)]) -> Vec<u8> {
        let mut body = offset.to_le_bytes().to_vec();
        body.extend_from_slice(&max.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0xFF_FFFF_FFFFu64.to_le_bytes());
        for &(low, high) in ranges {
            body.extend_from_slice(&[1, 22]);
            body.extend_from_slice(&low.to_le_bytes());
            body.extend_from_slice(&high.to_le_bytes());
            body.extend_from_slice(&4u32.to_le_bytes());
            body.extend_from_slice(&(1u64 << 34).to_le_bytes());
        }
        table(b"MSCT", 1, &body)
    }

    #[test]
    fn msct_ranges_cover_every_domain_in_order() {
        let t = msct(56, 3, &[(0, 1), (2, 3)]);
        let m = Msct::parse(&t).unwrap();
        assert_eq!(m.max_proximity_domain, 3);
        assert_eq!(m.ranges().count(), 2);
        assert!(
            Msct::parse(&msct(56, 3, &[(0, 7)])).is_ok(),
            "a range past the maximum"
        );
        let field = |t: Vec<u8>| match Msct::parse(&t).unwrap_err().kind {
            ErrorKind::Value { field } | ErrorKind::Offset { field } => field,
            k => panic!("{k:?}"),
        };
        assert_eq!(
            field(msct(56, 3, &[(0, 1)])),
            "a proximity domain no range covers"
        );
        assert_eq!(
            field(msct(56, 3, &[(0, 1), (3, 3)])),
            "a proximity domain no range covers"
        );
        assert_eq!(
            field(msct(56, 3, &[(0, 1), (2, 3), (1, 1)])),
            "Proximity Domain Range (low)"
        );
        assert_eq!(
            field(msct(56, 3, &[(0, 2), (2, 3)])),
            "Proximity Domain Range (low)"
        );
        assert_eq!(
            field(msct(56, 3, &[(1, 0)])),
            "Proximity Domain Range (high)"
        );
        assert_eq!(field(msct(52, 0, &[(0, 0)])), "OffsetProxDomInfo");
        assert_eq!(field(msct(200, 0, &[(0, 0)])), "OffsetProxDomInfo");
        assert!(Msct::parse(&msct(56, u32::MAX, &[(0, u32::MAX)])).is_ok());
    }
}
