//! The System Resource Affinity Table, signature `SRAT` (ACPI 6.6 §5.2.16).
//!
//! Decoded: the processor local APIC affinity (type 0), the memory affinity (1), the processor
//! local x2APIC affinity (2), the generic initiator affinity (5) and the generic port affinity (6).
//! The GICC, GIC ITS and RINTC affinities (3, 4, 7) are walked and listed, not decoded
//! (ADR-0019 A1); so are types 8 and above, which ACPI 6.6 does not define.

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

    /// Every structure in order, decoded and checked; the walk ends at the first refusal.
    pub fn entries(&self) -> impl Iterator<Item = Result<Affinity<'a>, Error>> + 'a {
        let signature = self.table.signature();
        let mut walk = self
            .table
            .reader()
            .rest(ENTRIES)
            .ok()
            .map(|run| Walk::new(run, Shape::BYTE_BYTE));
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            let item = walk.as_mut()?.next()?;
            let entry = item.and_then(|raw| decode(&raw, signature));
            done = entry.is_err();
            Some(entry)
        })
    }

    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        for raw in Walk::new(self.table.reader().rest(ENTRIES)?, Shape::BYTE_BYTE) {
            let raw = raw?;
            decode(&raw, self.table.signature())?;
            tally.structure(raw.kind, raw.at, raw.bytes);
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
            if base.checked_add(length).is_none() {
                // A range that wraps past the top of the address space names no memory.
                return Err(b.error(16, ErrorKind::Value { field: "Length" }));
            }
            let flags = b.u32(28)?;
            Affinity::Memory {
                proximity_domain: b.u32(2)?,
                base,
                length,
                enabled: flags & 1 != 0,
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

    #[test]
    fn malformed_structures_are_refused() {
        let wraps = srat(&[&memory(0, u64::MAX - 1, 2, 1)]);
        let e = census::check(&wraps).unwrap_err();
        assert_eq!(
            (e.at, e.kind),
            (48 + 16, ErrorKind::Value { field: "Length" })
        );
        for (kind, min) in [(0u8, 16u8), (1, 40), (2, 24), (5, 32), (6, 32)] {
            let mut short = vec![kind, min - 1];
            short.resize(usize::from(min - 1), 0);
            assert!(
                matches!(
                    census::check(&srat(&[&short])).unwrap_err().kind,
                    ErrorKind::Structure { .. }
                ),
                "type {kind}"
            );
        }
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
}
