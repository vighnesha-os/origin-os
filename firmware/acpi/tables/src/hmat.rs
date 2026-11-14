//! The Heterogeneous Memory Attribute Table, signature `HMAT` (ACPI 6.6 §5.2.29): memory
//! proximity domains' attributes, the latency and bandwidth between initiators and targets, and
//! memory-side caches.

use crate::census::Tally;
use crate::error::{Error, ErrorKind, Signature};
use crate::header::{self, Table};
use crate::read::Bytes;
use crate::walk::{Raw, Shape, Walk, Width, body};

/// Where the first structure is, after four bytes that align the structures to eight.
const ENTRIES: usize = 40;

/// An HMAT structure's shape: a 16-bit type, two reserved bytes, a 32-bit length.
const STRUCTURE: Shape = Shape {
    kind_at: 0,
    kind: Width::U16,
    len_at: 4,
    len: Width::U32,
};

/// A latency and bandwidth matrix (type 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Locality<'a> {
    /// Bits 0 to 3 the memory hierarchy level (0 memory, 1 to 3 a cache level); bits 4 and 5 the
    /// access attributes.
    pub flags: u8,
    /// What the entries measure: 0 to 2 latencies, 3 to 5 bandwidths; others reserved.
    pub data_type: u8,
    /// The transfer size the values hold at: 0 any, else 2 to the power of this plus 5 bytes.
    pub min_transfer_size: u8,
    /// Picoseconds or MB/s per unit of an entry.
    pub entry_base_unit: u64,
    initiators: Bytes<'a>,
    targets: Bytes<'a>,
    entries: Bytes<'a>,
}

impl Locality<'_> {
    /// How many initiators and targets.
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        (self.initiators.len() / 4, self.targets.len() / 4)
    }

    /// The `i`th initiator's proximity domain.
    #[must_use]
    pub fn initiator(&self, i: usize) -> Option<u32> {
        self.initiators.u32(i.checked_mul(4)?).ok()
    }

    /// The `j`th target's proximity domain.
    #[must_use]
    pub fn target(&self, j: usize) -> Option<u32> {
        self.targets.u32(j.checked_mul(4)?).ok()
    }

    /// The entry from initiator `i` to target `j`: `Some(None)` if they cannot reach each other
    /// (0xFFFF), else the value in base units.
    #[must_use]
    pub fn entry(&self, i: usize, j: usize) -> Option<Option<u16>> {
        let (_, t) = self.dimensions();
        if j >= t {
            return None;
        }
        let cell = i.checked_mul(t)?.checked_add(j)?.checked_mul(2)?;
        let v = self.entries.u16(cell).ok()?;
        Some((v != 0xFFFF).then_some(v))
    }
}

/// One HMAT structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HmatEntry<'a> {
    /// A memory proximity domain's attributes (type 0).
    MemoryDomain {
        /// The attached initiator's domain, if its flag says it is valid.
        initiator: Option<u32>,
        /// The memory's domain.
        memory: u32,
    },
    /// Latency or bandwidth between initiators and targets (type 1).
    Locality(Locality<'a>),
    /// A memory-side cache (type 2).
    Cache {
        /// The memory's domain.
        memory: u32,
        /// The cache's size in bytes.
        size: u64,
        /// The cache's attributes: levels, level, associativity, write policy, line size.
        attributes: u32,
        /// 1: extended-linear addressing.
        address_mode: u16,
        /// The SMBIOS handles of its devices, two bytes each.
        smbios_handles: &'a [u8],
    },
    /// A type ACPI 6.6 reserves.
    Reserved(Raw<'a>),
}

fn decode<'a>(raw: &Raw<'a>, signature: Signature) -> Result<HmatEntry<'a>, Error> {
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 40)?;
            HmatEntry::MemoryDomain {
                initiator: (b.u16(8)? & 1 != 0).then_some(b.u32(12)?),
                memory: b.u32(16)?,
            }
        }
        1 => {
            let b = body(raw, signature, 32)?;
            let s = usize::try_from(b.u32(12)?).unwrap_or(usize::MAX);
            let t = usize::try_from(b.u32(16)?).unwrap_or(usize::MAX);
            let initiators = b.array_of(32, s, 4, ("Number of Initiator Proximity Domains", 12))?;
            let targets_at = 32usize.saturating_add(initiators.len());
            let targets =
                b.array_of(targets_at, t, 4, ("Number of Target Proximity Domains", 16))?;
            let entries_at = targets_at.saturating_add(targets.len());
            let cells = s.checked_mul(t).ok_or_else(|| {
                b.error(
                    12,
                    ErrorKind::Offset {
                        field: "Number of Initiator Proximity Domains",
                    },
                )
            })?;
            let entries = b.array_of(entries_at, cells, 2, ("Entry", 12))?;
            let entry_base_unit = b.u64(24)?;
            if entry_base_unit == 0 {
                return Err(b.error(
                    24,
                    ErrorKind::Value {
                        field: "Entry Base Unit",
                    },
                ));
            }
            HmatEntry::Locality(Locality {
                flags: b.u8(8)?,
                data_type: b.u8(9)?,
                min_transfer_size: b.u8(10)?,
                entry_base_unit,
                initiators,
                targets,
                entries,
            })
        }
        2 => {
            let b = body(raw, signature, 32)?;
            let attributes = b.u32(24)?;
            let address_mode = b.u16(28)?;
            // Extended-linear addressing needs a direct-mapped cache (§5.2.29.5).
            if address_mode == 1 && (attributes >> 8) & 0xF != 1 {
                return Err(b.error(
                    28,
                    ErrorKind::Inconsistent {
                        field: "Address Mode",
                    },
                ));
            }
            let n = usize::from(b.u16(30)?);
            HmatEntry::Cache {
                memory: b.u32(8)?,
                size: b.u64(16)?,
                attributes,
                address_mode,
                smbios_handles: b
                    .array_of(32, n, 2, ("Number of SMBIOS handles", 30))?
                    .bytes(),
            }
        }
        _ => HmatEntry::Reserved(*raw),
    })
}

/// The checked HMAT.
#[derive(Clone, Copy, Debug)]
pub struct Hmat<'a> {
    table: Table<'a>,
}

impl<'a> Hmat<'a> {
    /// Checks the table. Refused besides each structure's own length: a matrix whose domain lists
    /// and entries do not fit its structure, a base unit of zero (§5.2.29.4), and extended-linear
    /// addressing on a cache that is not direct-mapped (§5.2.29.5).
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"HMAT")?;
        table.reader().require(ENTRIES)?;
        let hmat = Self { table };
        for entry in hmat.entries() {
            entry?;
        }
        Ok(hmat)
    }

    /// Every structure, in order.
    pub fn entries(&self) -> impl Iterator<Item = Result<HmatEntry<'a>, Error>> + 'a {
        let signature = self.table.signature();
        self.table
            .reader()
            .rest(ENTRIES)
            .ok()
            .into_iter()
            .flat_map(|run| Walk::new(run, STRUCTURE))
            .map(move |raw| decode(&raw?, signature))
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(ENTRIES)?, STRUCTURE) {
            let raw = raw?;
            tally.structure(raw.kind, raw.at, raw.bytes);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        vec![hmat(&[
            domain(Some(0), 1),
            locality(&[0, 1], &[0, 1, 2], &[10, 20, 0xFFFF, 30, 40, 50], 1000),
            cache(0x0040_1111, 1, &[0x10, 0x11]),
        ])]
    }

    fn s(kind: u16, body: &[u8]) -> Vec<u8> {
        let mut e = kind.to_le_bytes().to_vec();
        e.extend_from_slice(&[0, 0]);
        e.extend_from_slice(&u32::try_from(body.len() + 8).unwrap().to_le_bytes());
        e.extend_from_slice(body);
        e
    }

    fn domain(initiator: Option<u32>, memory: u32) -> Vec<u8> {
        let mut b = vec![0u8; 32];
        b[0] = u8::from(initiator.is_some());
        b[4..8].copy_from_slice(&initiator.unwrap_or(0).to_le_bytes());
        b[8..12].copy_from_slice(&memory.to_le_bytes());
        s(0, &b)
    }

    fn locality(si: &[u32], ti: &[u32], cells: &[u16], unit: u64) -> Vec<u8> {
        let mut b = vec![0, 0, 0, 0];
        b.extend_from_slice(&u32::try_from(si.len()).unwrap().to_le_bytes());
        b.extend_from_slice(&u32::try_from(ti.len()).unwrap().to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&unit.to_le_bytes());
        for d in si.iter().chain(ti) {
            b.extend_from_slice(&d.to_le_bytes());
        }
        for c in cells {
            b.extend_from_slice(&c.to_le_bytes());
        }
        s(1, &b)
    }

    fn cache(attributes: u32, mode: u16, handles: &[u16]) -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[8..16].copy_from_slice(&(1u64 << 30).to_le_bytes());
        b[16..20].copy_from_slice(&attributes.to_le_bytes());
        b[20..22].copy_from_slice(&mode.to_le_bytes());
        b[22..24].copy_from_slice(&u16::try_from(handles.len()).unwrap().to_le_bytes());
        for h in handles {
            b.extend_from_slice(&h.to_le_bytes());
        }
        s(2, &b)
    }

    fn hmat(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut body = vec![0u8; 4];
        for e in entries {
            body.extend_from_slice(e);
        }
        table(b"HMAT", 2, &body)
    }

    #[test]
    fn decodes_every_structure() {
        let t = hmat(&[
            domain(Some(0), 1),
            locality(&[0, 1], &[0, 1, 2], &[10, 20, 0xFFFF, 30, 40, 50], 1000),
            cache(0x0040_1111, 1, &[0x10, 0x11]),
            s(9, &[]),
        ]);
        let h = Hmat::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        let all: Vec<_> = h.entries().map(Result::unwrap).collect();
        assert_eq!(
            all[0],
            HmatEntry::MemoryDomain {
                initiator: Some(0),
                memory: 1
            }
        );
        let HmatEntry::Locality(l) = all[1] else {
            panic!()
        };
        assert_eq!(l.dimensions(), (2, 3));
        assert_eq!(
            (l.initiator(1), l.target(2), l.target(3)),
            (Some(1), Some(2), None)
        );
        assert_eq!(
            (l.entry(0, 1), l.entry(0, 2), l.entry(1, 2), l.entry(0, 3)),
            (Some(Some(20)), Some(None), Some(Some(50)), None)
        );
        assert!(
            matches!(all[2], HmatEntry::Cache { address_mode: 1, smbios_handles, .. } if smbios_handles.len() == 4)
        );
        assert!(
            matches!(census::check(&t).unwrap().class, census::Class::Checked(s) if s.structures() == 4)
        );
    }

    #[test]
    fn sizes_and_rules_are_checked() {
        let kind = |e: Vec<u8>| Hmat::parse(&hmat(&[e])).unwrap_err().kind;
        assert_eq!(
            kind(locality(&[0], &[0], &[1], 0)),
            ErrorKind::Value {
                field: "Entry Base Unit"
            }
        );
        assert!(matches!(
            kind(locality(&[0, 1], &[0], &[1], 1)),
            ErrorKind::Offset { field: "Entry" }
        ));
        let mut huge = locality(&[0], &[0], &[1], 1);
        huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(kind(huge), ErrorKind::Offset { .. }));
        assert_eq!(
            kind(cache(0x0040_1211, 1, &[])),
            ErrorKind::Inconsistent {
                field: "Address Mode"
            }
        );
        let mut handles = cache(0, 0, &[1]);
        handles[30] = 5;
        assert!(matches!(kind(handles), ErrorKind::Offset { .. }));
        assert!(matches!(kind(s(0, &[0; 31])), ErrorKind::Structure { .. }));
        assert!(matches!(
            kind(vec![1, 0, 0, 0, 7, 0, 0, 0]),
            ErrorKind::Structure { .. }
        ));
    }
}
