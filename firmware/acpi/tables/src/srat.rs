//! The System Resource Affinity Table, signature `SRAT` (ACPI 6.6 §5.2.16).
//!
//! Decoded: the processor local APIC affinity (type 0), the memory affinity (1), the processor
//! local x2APIC affinity (2), the generic initiator affinity (5) and the generic port affinity (6).
//! The GICC, GIC ITS and RINTC affinities (3, 4, 7) are walked and listed, not decoded
//! (ADR-0019 A1); so are types 8 and above, which ACPI 6.6 does not define.
//!
//! **Verdicts are per structure**, as the MADT's (the review's F4 and F5, and the coordinating
//! session's ruling): a structure too short for its type, or an enabled memory range that wraps
//! past the top of the address space, is reported and not used, and the walk goes on. A disabled
//! memory affinity structure is accepted whatever its range: §5.2.16.2 tells the OS to ignore
//! its contents, so that firmware can carry placeholders. Only a length that cannot locate the
//! next structure stops the walk, and so refuses the table.

use crate::census::Tally;
use crate::error::{Error, ErrorKind};
use crate::header::{self, Table};
use crate::read::Bytes;
use crate::walk::{Raw, Shape, Walk, body};

/// Where the first structure is.
const ENTRIES: usize = 48;

/// A device named by a generic initiator or port (§5.2.16.6, Tables 5.75 and 5.76).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceHandle {
    /// A device in the namespace, by its `_HID` and `_UID` (handle type 0).
    Acpi {
        /// Its `_HID`, as the table holds it.
        hid: [u8; 8],
        /// Its `_UID`.
        uid: u32,
    },
    /// A PCI function (handle type 1). The bus is the handle's third byte and the device and
    /// function its fourth, as Table 5.76 places them.
    Pci {
        /// Its segment group.
        segment: u16,
        /// Its bus.
        bus: u8,
        /// Its device.
        device: u8,
        /// Its function.
        function: u8,
    },
    /// A handle type ACPI 6.6 reserves (2 and above).
    Reserved(u8),
}

/// One affinity structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity<'a> {
    /// A processor by its local APIC ID (type 0).
    LocalApic {
        /// Its proximity domain, from byte 2 and bytes 9 to 11.
        proximity_domain: u32,
        /// Its local APIC ID.
        apic_id: u8,
        /// The structure counts: if not, ACPI 6.6 tells the OS to ignore it.
        enabled: bool,
        /// Its clock domain.
        clock_domain: u32,
    },
    /// A range of memory (type 1).
    Memory {
        /// Its proximity domain.
        proximity_domain: u32,
        /// Its first byte's physical address.
        base: u64,
        /// Its length in bytes.
        length: u64,
        /// The structure counts.
        enabled: bool,
        /// The range can be added and removed while the system runs.
        hot_pluggable: bool,
        /// The memory is non-volatile.
        non_volatile: bool,
        /// The memory is meant for a specific purpose (`EFI_MEMORY_SP`).
        specific_purpose: bool,
    },
    /// A processor by its x2APIC ID (type 2).
    X2Apic {
        /// Its proximity domain.
        proximity_domain: u32,
        /// Its x2APIC ID.
        x2apic_id: u32,
        /// The structure counts.
        enabled: bool,
        /// Its clock domain.
        clock_domain: u32,
    },
    /// A device that initiates memory transactions, not a processor (type 5).
    GenericInitiator {
        /// Its proximity domain.
        proximity_domain: u32,
        /// The device.
        handle: DeviceHandle,
        /// The structure counts.
        enabled: bool,
        /// It makes every kind of transaction the host's processors make.
        architectural_transactions: bool,
    },
    /// A port into a topology the OS discovers (a CXL host bridge, say) (type 6).
    GenericPort {
        /// The proximity domain the HMAT describes it by.
        proximity_domain: u32,
        /// The device.
        handle: DeviceHandle,
        /// The structure counts.
        enabled: bool,
        /// It makes every kind of transaction the host's processors make.
        architectural_transactions: bool,
    },
    /// A structure for another processor architecture (ADR-0019 A1).
    NotX86(Raw<'a>),
    /// A type ACPI 6.6 does not define.
    Reserved(Raw<'a>),
}

/// The checked SRAT.
#[derive(Clone, Copy, Debug)]
pub struct Srat<'a> {
    table: Table<'a>,
}

impl<'a> Srat<'a> {
    /// Checks the table; its structures come from [`Srat::entries`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"SRAT")?;
        table.reader().require(ENTRIES)?;
        Ok(Self { table })
    }

    /// The table.
    #[must_use]
    pub const fn table(&self) -> &Table<'a> {
        &self.table
    }

    /// Every structure in order, decoded and checked. A structure whose contents break a rule
    /// comes back as `Err` and the walk goes on past it; a length that cannot locate the next
    /// structure comes back as `Err` and ends the walk (the review's F5, F15).
    pub fn entries(&self) -> impl Iterator<Item = Result<Affinity<'a>, Error>> + 'a {
        let signature = self.table.signature();
        let mut walk = self
            .table
            .reader()
            .rest(ENTRIES)
            .map(|run| Walk::new(run, Shape::BYTE_BYTE));
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            let item = match walk.as_mut() {
                Ok(w) => w.next()?,
                Err(e) => Err(*e),
            };
            Some(match item {
                Ok(raw) => decode(&raw, signature),
                Err(e) => {
                    done = true;
                    Err(e)
                }
            })
        })
    }

    /// Checks every structure: one that breaks a rule is reported and not used; a walk error
    /// refuses the table.
    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(ENTRIES)?, Shape::BYTE_BYTE) {
            let raw = raw?;
            match decode(&raw, self.table.signature()) {
                Ok(_) => tally.structure(raw.kind, raw.at, raw.bytes),
                Err(e) => tally.reported(e, raw.at, raw.bytes),
            }
        }
        Ok(())
    }
}

fn handle(b: &Bytes<'_>, kind: u8) -> Result<DeviceHandle, Error> {
    Ok(match kind {
        0 => DeviceHandle::Acpi {
            hid: b.array(8)?,
            uid: b.u32(16)?,
        },
        1 => {
            let dev_fn = b.u8(11)?;
            DeviceHandle::Pci {
                segment: b.u16(8)?,
                bus: b.u8(10)?,
                device: dev_fn >> 3,
                function: dev_fn & 0b111,
            }
        }
        _ => DeviceHandle::Reserved(kind),
    })
}

fn decode<'a>(raw: &Raw<'a>, signature: crate::Signature) -> Result<Affinity<'a>, Error> {
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 16)?;
            let low = u32::from(b.u8(2)?);
            let [h0, h1, h2] = b.array::<3>(9)?;
            let high = u32::from_le_bytes([h0, h1, h2, 0]);
            Affinity::LocalApic {
                proximity_domain: low | high << 8,
                apic_id: b.u8(3)?,
                enabled: b.u32(4)? & 1 != 0,
                clock_domain: b.u32(12)?,
            }
        }
        1 => {
            let b = body(raw, signature, 40)?;
            let base = b.u64(8)?;
            let length = b.u64(16)?;
            let flags = b.u32(28)?;
            let enabled = flags & 1 != 0;
            if enabled && base.checked_add(length).is_none() {
                // A range that wraps past the top of the address space names no memory. A
                // disabled structure's contents are ignored (§5.2.16.2, the review's F4).
                return Err(b.error(16, ErrorKind::Value { field: "Length" }));
            }
            Affinity::Memory {
                proximity_domain: b.u32(2)?,
                base,
                length,
                enabled,
                hot_pluggable: flags & 2 != 0,
                non_volatile: flags & 4 != 0,
                specific_purpose: flags & 8 != 0,
            }
        }
        2 => {
            let b = body(raw, signature, 24)?;
            Affinity::X2Apic {
                proximity_domain: b.u32(4)?,
                x2apic_id: b.u32(8)?,
                enabled: b.u32(12)? & 1 != 0,
                clock_domain: b.u32(16)?,
            }
        }
        5 | 6 => {
            let b = body(raw, signature, 32)?;
            let handle = handle(&b, b.u8(3)?)?;
            let flags = b.u32(24)?;
            let (proximity_domain, enabled, architectural_transactions) =
                (b.u32(4)?, flags & 1 != 0, flags & 2 != 0);
            if raw.kind == 5 {
                Affinity::GenericInitiator {
                    proximity_domain,
                    handle,
                    enabled,
                    architectural_transactions,
                }
            } else {
                Affinity::GenericPort {
                    proximity_domain,
                    handle,
                    enabled,
                    architectural_transactions,
                }
            }
        }
        3 | 4 | 7 => Affinity::NotX86(*raw),
        _ => Affinity::Reserved(*raw),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        let mut pci = [0u8; 16];
        pci[2] = 0x3A;
        vec![srat(&[
            &lapic(1, 7),
            &memory(1, 0x1_0000_0000, 0x4000_0000, 0b1011),
            &x2apic(2, 300),
            &generic(5, 1, 3, pci),
            &generic(6, 0, 4, [0x41; 16]),
        ])]
    }

    pub(crate) fn srat(entries: &[&[u8]]) -> Vec<u8> {
        let mut body = vec![0u8; 12];
        body[0] = 1;
        for e in entries {
            body.extend_from_slice(e);
        }
        table(b"SRAT", 3, &body)
    }

    pub(crate) fn lapic(domain: u32, apic_id: u8) -> Vec<u8> {
        let d = domain.to_le_bytes();
        let mut e = vec![0, 16, d[0], apic_id, 1, 0, 0, 0, 0, d[1], d[2], d[3]];
        e.extend_from_slice(&7u32.to_le_bytes());
        e
    }

    pub(crate) fn memory(domain: u32, base: u64, length: u64, flags: u32) -> Vec<u8> {
        let mut e = vec![1, 40];
        e.extend_from_slice(&domain.to_le_bytes());
        e.extend_from_slice(&[0, 0]);
        e.extend_from_slice(&base.to_le_bytes());
        e.extend_from_slice(&length.to_le_bytes());
        e.extend_from_slice(&[0; 4]);
        e.extend_from_slice(&flags.to_le_bytes());
        e.extend_from_slice(&[0; 8]);
        e
    }

    pub(crate) fn x2apic(domain: u32, id: u32) -> Vec<u8> {
        let mut e = vec![2, 24, 0, 0];
        e.extend_from_slice(&domain.to_le_bytes());
        e.extend_from_slice(&id.to_le_bytes());
        e.extend_from_slice(&1u32.to_le_bytes());
        e.extend_from_slice(&3u32.to_le_bytes());
        e.extend_from_slice(&[0; 4]);
        e
    }

    pub(crate) fn generic(kind: u8, handle_type: u8, domain: u32, handle: [u8; 16]) -> Vec<u8> {
        let mut e = vec![kind, 32, 0, handle_type];
        e.extend_from_slice(&domain.to_le_bytes());
        e.extend_from_slice(&handle);
        e.extend_from_slice(&3u32.to_le_bytes());
        e.extend_from_slice(&[0; 4]);
        e
    }

    #[test]
    fn decodes_every_x86_structure() {
        let mut acpi = [0u8; 16];
        acpi[..8].copy_from_slice(b"ACPI0016");
        acpi[8..12].copy_from_slice(&5u32.to_le_bytes());
        let mut pci = [0u8; 16];
        pci[0..2].copy_from_slice(&1u16.to_le_bytes());
        pci[2] = 0x3A;
        pci[3] = (4 << 3) | 2;
        let mut gicc = vec![3, 18];
        gicc.resize(18, 0);
        let t = srat(&[
            &lapic(0x0012_3456, 7),
            &memory(1, 0x1_0000_0000, 0x4000_0000, 0b1011),
            &x2apic(2, 300),
            &generic(5, 1, 3, pci),
            &generic(6, 0, 4, acpi),
            &gicc,
        ]);
        let s = Srat::parse(&t).unwrap();
        let all: Vec<_> = s.entries().map(Result::unwrap).collect();
        assert_eq!(
            all[0],
            Affinity::LocalApic {
                proximity_domain: 0x0012_3456,
                apic_id: 7,
                enabled: true,
                clock_domain: 7
            }
        );
        assert_eq!(
            all[1],
            Affinity::Memory {
                proximity_domain: 1,
                base: 0x1_0000_0000,
                length: 0x4000_0000,
                enabled: true,
                hot_pluggable: true,
                non_volatile: false,
                specific_purpose: true
            }
        );
        assert!(matches!(
            all[2],
            Affinity::X2Apic {
                x2apic_id: 300,
                proximity_domain: 2,
                ..
            }
        ));
        assert_eq!(
            all[3],
            Affinity::GenericInitiator {
                proximity_domain: 3,
                handle: DeviceHandle::Pci {
                    segment: 1,
                    bus: 0x3A,
                    device: 4,
                    function: 2
                },
                enabled: true,
                architectural_transactions: true
            }
        );
        assert!(matches!(
            all[4],
            Affinity::GenericPort {
                handle: DeviceHandle::Acpi { uid: 5, .. },
                ..
            }
        ));
        assert!(matches!(all[5], Affinity::NotX86(Raw { kind: 3, .. })));
        let c = census::check(&t).unwrap();
        assert!(matches!(c.class, census::Class::Checked(s) if s.structures() == 6));
    }

    /// The census's summary of `t`, which it must accept.
    fn summary(t: &[u8]) -> census::Summary {
        match census::check(t).unwrap().class {
            census::Class::Checked(s) => s,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn malformed_structures_are_reported_and_not_used() {
        let wraps = srat(&[&memory(0, u64::MAX - 1, 2, 1)]);
        let s = summary(&wraps);
        assert_eq!((s.reported(), s.structures()), (1, 0));
        let e = s.notes().next().unwrap();
        assert_eq!(
            (e.at, e.kind),
            (48 + 16, ErrorKind::Value { field: "Length" })
        );
        for (kind, min) in [(0u8, 16u8), (1, 40), (2, 24), (5, 32), (6, 32)] {
            let mut short = vec![kind, min - 1];
            short.resize(usize::from(min - 1), 0);
            let s = summary(&srat(&[&short]));
            assert!(
                matches!(s.notes().next().unwrap().kind, ErrorKind::Structure { .. }),
                "type {kind}"
            );
        }
        // A walk error still refuses the table.
        assert!(census::check(&srat(&[&lapic(0, 1), &[9, 0]])).is_err());
        let mut reserved_handle = generic(5, 9, 0, [0; 16]);
        reserved_handle[24] = 1;
        let t = srat(&[&reserved_handle]);
        assert!(matches!(
            Srat::parse(&t).unwrap().entries().next(),
            Some(Ok(Affinity::GenericInitiator {
                handle: DeviceHandle::Reserved(9),
                ..
            }))
        ));
        assert!(matches!(
            Srat::parse(&table(b"SRAT", 3, &[0; 11])).unwrap_err().kind,
            ErrorKind::TooShort { .. }
        ));
    }

    /// The review's F4: a disabled memory affinity structure is accepted whatever its range, and
    /// the processors and memory after a bad structure are kept (F5).
    #[test]
    fn a_disabled_placeholder_is_ignored_and_what_follows_is_kept() {
        let placeholder = memory(0, 0xFFFF_FFFF_0000_0000, 0x1_0000_0000_0000, 0);
        let t = srat(&[&placeholder, &lapic(0, 1)]);
        assert_eq!(summary(&t).reported(), 0);
        let all: Vec<_> = Srat::parse(&t).unwrap().entries().collect();
        assert!(matches!(
            all[0],
            Ok(Affinity::Memory { enabled: false, .. })
        ));
        let mut short_lapic = vec![0u8, 15];
        short_lapic.resize(15, 0);
        let bad = srat(&[
            &memory(0, u64::MAX - 1, 2, 1),
            &short_lapic,
            &x2apic(1, 300),
            &memory(1, 0x1_0000_0000, 0x4000_0000, 1),
        ]);
        let s = summary(&bad);
        assert_eq!((s.reported(), s.structures()), (2, 2));
        let all: Vec<_> = Srat::parse(&bad).unwrap().entries().collect();
        assert_eq!(all.len(), 4);
        assert!(all[0].is_err() && all[1].is_err());
        assert!(matches!(
            all[2],
            Ok(Affinity::X2Apic { x2apic_id: 300, .. })
        ));
        assert!(matches!(
            all[3],
            Ok(Affinity::Memory {
                proximity_domain: 1,
                ..
            })
        ));
    }
}
