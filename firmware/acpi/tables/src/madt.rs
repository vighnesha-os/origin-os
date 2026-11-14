//! The Multiple APIC Description Table, signature `APIC` (ACPI 6.6 §5.2.12), and the rules of
//! §5.2.13 on global system interrupts that the table alone can show.
//!
//! Every x86 structure is decoded: the processor local APIC (type 0), the I/O APIC (1), the
//! interrupt source override (2), the NMI source (3), the local APIC NMI (4), the local APIC
//! address override (5), the processor local x2APIC (9), the local x2APIC NMI (10) and the
//! multiprocessor wakeup structure (16), whose mailbox is located and never touched. The
//! structures for Itanium (6 to 8), Arm (11 to 15), LoongArch (17 to 23) and RISC-V (24 to 27)
//! are walked by their length and listed, not decoded (ADR-0019 A1); so are types 28 and above,
//! which ACPI 6.6 leaves to OEMs.
//!
//! A structure longer than its type's layout is accepted and its extra bytes ignored (§5.2.2:
//! layouts grow by appending); a shorter one is refused.

use crate::census::Tally;
use crate::error::{Error, ErrorKind};
use crate::header::{self, Table};
use crate::read::Bytes;
use crate::walk::{Raw, Shape, Walk, body};

/// Where the first interrupt controller structure is.
const ENTRIES: usize = 44;

/// The polarity and trigger mode of an interrupt input (§5.2.12.5, the MPS INTI flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inti {
    /// The input's polarity.
    pub polarity: Polarity,
    /// The input's trigger mode.
    pub trigger: Trigger,
}

/// An input's polarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Polarity {
    /// As the bus's specification gives it (ISA: active high).
    BusDefault,
    /// Active high.
    ActiveHigh,
    /// Active low.
    ActiveLow,
}

/// An input's trigger mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// As the bus's specification gives it (ISA: edge).
    BusDefault,
    /// Edge-triggered.
    Edge,
    /// Level-triggered.
    Level,
}

impl Inti {
    /// Decodes the 16-bit flags. The reserved encoding of either field (binary 10) is refused: an
    /// input programmed from it would have no defined polarity or trigger. Bits 4 to 15 are
    /// reserved and ignored (§5.2.1.1).
    fn read(b: &Bytes<'_>, at: usize, field: &'static str) -> Result<Self, Error> {
        let flags = b.u16(at)?;
        let refuse = || b.error(at, ErrorKind::Value { field });
        let polarity = match flags & 0b11 {
            0b00 => Polarity::BusDefault,
            0b01 => Polarity::ActiveHigh,
            0b11 => Polarity::ActiveLow,
            _ => return Err(refuse()),
        };
        let trigger = match (flags >> 2) & 0b11 {
            0b00 => Trigger::BusDefault,
            0b01 => Trigger::Edge,
            0b11 => Trigger::Level,
            _ => return Err(refuse()),
        };
        Ok(Self { polarity, trigger })
    }
}

/// A processor's local APIC flags (§5.2.12.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessorFlags {
    /// Ready for use.
    pub enabled: bool,
    /// Not enabled, and the hardware can bring it online while the system runs. ACPI 6.6 reserves
    /// the bit when the processor is enabled, so it is read only when it is not.
    pub online_capable: bool,
}

impl ProcessorFlags {
    const fn from_bits(flags: u32) -> Self {
        let enabled = flags & 1 != 0;
        Self {
            enabled,
            online_capable: !enabled && flags & 2 != 0,
        }
    }

    /// Whether ACPI 6.6 tells the OS to ignore the structure: neither enabled nor online capable.
    #[must_use]
    pub const fn ignored(&self) -> bool {
        !self.enabled && !self.online_capable
    }
}

/// One interrupt controller structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry<'a> {
    /// A processor's local APIC (type 0).
    LocalApic {
        /// Its ACPI processor UID.
        uid: u8,
        /// Its local APIC ID.
        apic_id: u8,
        /// Its flags.
        flags: ProcessorFlags,
    },
    /// An I/O APIC (type 1).
    IoApic {
        /// Its I/O APIC ID.
        id: u8,
        /// The physical address of its registers.
        address: u32,
        /// The global system interrupt of its first input.
        gsi_base: u32,
    },
    /// An ISA interrupt delivered as another global system interrupt (type 2).
    SourceOverride {
        /// The ISA IRQ.
        source: u8,
        /// The global system interrupt it arrives as.
        gsi: u32,
        /// Its polarity and trigger mode.
        flags: Inti,
    },
    /// An I/O APIC input to be programmed as an NMI (type 3).
    NmiSource {
        /// Its polarity and trigger mode.
        flags: Inti,
        /// The global system interrupt.
        gsi: u32,
    },
    /// A local APIC input wired to NMI (type 4).
    LocalApicNmi {
        /// The processor's UID, or `None` for every processor (UID 0xFF).
        uid: Option<u8>,
        /// Its polarity and trigger mode.
        flags: Inti,
        /// Which local input: 0 for LINT0, 1 for LINT1.
        lint: u8,
    },
    /// The local APIC's 64-bit address, in place of the header's (type 5).
    LocalApicAddressOverride {
        /// The address.
        address: u64,
    },
    /// A processor's local x2APIC (type 9).
    LocalX2Apic {
        /// Its x2APIC ID.
        x2apic_id: u32,
        /// Its flags.
        flags: ProcessorFlags,
        /// Its ACPI processor UID.
        uid: u32,
    },
    /// A local x2APIC input wired to NMI (type 10).
    LocalX2ApicNmi {
        /// Its polarity and trigger mode.
        flags: Inti,
        /// The processor's UID, or `None` for every processor (UID 0xFFFFFFFF).
        uid: Option<u32>,
        /// Which local input: 0 for LINT0, 1 for LINT1.
        lint: u8,
    },
    /// The mailbox through which application processors are woken (type 16, §5.2.12.19). Located
    /// only: nothing here reads or writes it.
    MultiprocessorWakeup {
        /// The mailbox protocol's version.
        mailbox_version: u16,
        /// The mailbox's physical address, 4 KiB-aligned.
        mailbox: u64,
        /// Where a processor can be sent to watch the mailbox again, if the firmware offers it
        /// (version 1 and later, a structure of 24 bytes).
        reset_vector: Option<u64>,
    },
    /// A structure for another processor architecture (ADR-0019 A1): walked, not decoded.
    NotX86(Raw<'a>),
    /// A type ACPI 6.6 leaves to OEMs (28 and above): walked, not decoded.
    Oem(Raw<'a>),
}

/// The checked MADT.
#[derive(Clone, Copy, Debug)]
pub struct Madt<'a> {
    table: Table<'a>,
    /// The 32-bit physical address of every processor's local APIC.
    pub local_apic_address: u32,
    /// The platform also has a PC-AT dual 8259, whose vectors must be masked when the APIC is used
    /// (`PCAT_COMPAT`).
    pub pcat_compat: bool,
}

impl<'a> Madt<'a> {
    /// Checks the table and reads its fixed fields; its structures come from [`Madt::entries`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"APIC")?;
        let b = table.reader();
        b.require(ENTRIES)?;
        Ok(Self {
            table,
            local_apic_address: b.u32(36)?,
            pcat_compat: b.u32(40)? & 1 != 0,
        })
    }

    /// The table.
    #[must_use]
    pub const fn table(&self) -> &Table<'a> {
        &self.table
    }

    /// Every structure, in the table's order, each decoded and checked; the walk ends at the first
    /// refusal.
    pub fn entries(&self) -> impl Iterator<Item = Result<Entry<'a>, Error>> + 'a {
        let b = self.table.reader();
        let run = b.rest(ENTRIES);
        let signature = b.table();
        let mut walk = run.map(|run| Walk::new(run, Shape::BYTE_BYTE));
        let mut failed = None;
        if let Err(e) = &walk {
            failed = Some(*e);
        }
        let mut done = false;
        core::iter::from_fn(move || {
            if done {
                return None;
            }
            if let Some(e) = failed.take() {
                done = true;
                return Some(Err(e));
            }
            let item = walk.as_mut().ok()?.next()?;
            let entry = item.and_then(|raw| decode(&raw, signature));
            done = entry.is_err();
            Some(entry)
        })
    }

    /// Every local APIC's physical address: the override's if there is one, else the header's.
    pub fn local_apic_base(&self) -> Result<u64, Error> {
        let mut base = u64::from(self.local_apic_address);
        for entry in self.entries() {
            if let Entry::LocalApicAddressOverride { address } = entry? {
                base = address;
            }
        }
        Ok(base)
    }

    /// Checks the whole table: every structure, and what holds between them. At most one local
    /// APIC address override (§5.2.12.8); no two I/O APICs at one address (§5.2.12.3); no two I/O
    /// APICs with one global system interrupt base, which would give them the same input
    /// (§5.2.13: each input is one I/O APIC's alone); and at most [`MAX_IO_APICS`], as many as
    /// 8-bit I/O APIC IDs can tell apart, which also bounds the work of the checks between them.
    pub(crate) fn census(&self, tally: &mut Tally<'_, 'a>) -> Result<(), Error> {
        let walk = Walk::new(self.table.reader().rest(ENTRIES)?, Shape::BYTE_BYTE);
        let signature = self.table.signature();
        let inconsistent = |at, field| Error::new(signature, at, ErrorKind::Inconsistent { field });
        let mut overrides = 0u32;
        let mut io_apics = [(0u32, 0u32); MAX_IO_APICS];
        let mut count = 0usize;
        for raw in walk {
            let raw = raw?;
            let entry = decode(&raw, signature)?;
            tally.structure(raw.kind, raw.at, raw.bytes);
            match entry {
                Entry::LocalApicAddressOverride { .. } => {
                    overrides = overrides.saturating_add(1);
                    if overrides > 1 {
                        return Err(inconsistent(raw.at, "a second local APIC address override"));
                    }
                }
                Entry::IoApic {
                    address, gsi_base, ..
                } => {
                    let seen = io_apics.get(..count).unwrap_or(&[]);
                    if seen.iter().any(|&(a, _)| a == address) {
                        return Err(inconsistent(raw.at, "I/O APIC address"));
                    }
                    if seen.iter().any(|&(_, g)| g == gsi_base) {
                        return Err(inconsistent(
                            raw.at,
                            "I/O APIC global system interrupt base",
                        ));
                    }
                    let slot = io_apics.get_mut(count).ok_or_else(|| {
                        Error::new(
                            signature,
                            raw.at,
                            ErrorKind::Value {
                                field: "more I/O APICs than 8-bit IDs tell apart",
                            },
                        )
                    })?;
                    *slot = (address, gsi_base);
                    count = count.saturating_add(1);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The most I/O APICs a MADT may list: as many as 8-bit I/O APIC IDs tell apart.
pub const MAX_IO_APICS: usize = 256;

/// Whether any two of `ranges`, each an I/O APIC's interrupt base and its number of inputs (which
/// the I/O APIC's own registers give, not the MADT), share an interrupt (§5.2.13).
#[must_use]
pub fn gsi_ranges_overlap(ranges: &[(u32, u32)]) -> bool {
    ranges.iter().enumerate().any(|(i, &(base, count))| {
        let end = u64::from(base).saturating_add(u64::from(count));
        ranges.iter().skip(i.saturating_add(1)).any(|&(b, c)| {
            let e = u64::from(b).saturating_add(u64::from(c));
            u64::from(base) < e && u64::from(b) < end && count != 0 && c != 0
        })
    })
}

/// Decodes one structure.
fn decode<'a>(raw: &Raw<'a>, signature: crate::Signature) -> Result<Entry<'a>, Error> {
    let lint = |b: &Bytes<'_>, at: usize| -> Result<u8, Error> {
        let lint = b.u8(at)?;
        if lint > 1 {
            // A local APIC has LINT0 and LINT1 only.
            return Err(b.error(at, ErrorKind::Value { field: "LINT#" }));
        }
        Ok(lint)
    };
    Ok(match raw.kind {
        0 => {
            let b = body(raw, signature, 8)?;
            Entry::LocalApic {
                uid: b.u8(2)?,
                apic_id: b.u8(3)?,
                flags: ProcessorFlags::from_bits(b.u32(4)?),
            }
        }
        1 => {
            let b = body(raw, signature, 12)?;
            Entry::IoApic {
                id: b.u8(2)?,
                address: b.u32(4)?,
                gsi_base: b.u32(8)?,
            }
        }
        2 => {
            let b = body(raw, signature, 10)?;
            if b.u8(2)? != 0 {
                // Only ISA (bus 0) interrupts can be overridden.
                return Err(b.error(2, ErrorKind::Value { field: "Bus" }));
            }
            Entry::SourceOverride {
                source: b.u8(3)?,
                gsi: b.u32(4)?,
                flags: Inti::read(&b, 8, "Flags")?,
            }
        }
        3 => {
            let b = body(raw, signature, 8)?;
            Entry::NmiSource {
                flags: Inti::read(&b, 2, "Flags")?,
                gsi: b.u32(4)?,
            }
        }
        4 => {
            let b = body(raw, signature, 6)?;
            let uid = b.u8(2)?;
            Entry::LocalApicNmi {
                uid: (uid != 0xFF).then_some(uid),
                flags: Inti::read(&b, 3, "Flags")?,
                lint: lint(&b, 5)?,
            }
        }
        5 => {
            let b = body(raw, signature, 12)?;
            Entry::LocalApicAddressOverride { address: b.u64(4)? }
        }
        9 => {
            let b = body(raw, signature, 16)?;
            Entry::LocalX2Apic {
                x2apic_id: b.u32(4)?,
                flags: ProcessorFlags::from_bits(b.u32(8)?),
                uid: b.u32(12)?,
            }
        }
        10 => {
            let b = body(raw, signature, 12)?;
            let uid = b.u32(4)?;
            Entry::LocalX2ApicNmi {
                flags: Inti::read(&b, 2, "Flags")?,
                uid: (uid != u32::MAX).then_some(uid),
                lint: lint(&b, 8)?,
            }
        }
        16 => {
            let b = body(raw, signature, 16)?;
            let mailbox_version = b.u16(2)?;
            let mailbox = b.u64(8)?;
            if mailbox & 0xFFF != 0 {
                // The mailbox is a 4 KiB page; an address inside a page names no mailbox.
                return Err(b.error(
                    8,
                    ErrorKind::Value {
                        field: "MailBoxAddress",
                    },
                ));
            }
            Entry::MultiprocessorWakeup {
                mailbox_version,
                mailbox,
                reset_vector: if mailbox_version >= 1 {
                    b.opt_u64(16)
                } else {
                    None
                },
            }
        }
        6..=8 | 11..=15 | 17..=27 => Entry::NotX86(*raw),
        _ => Entry::Oem(*raw),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::census;
    use crate::testing::table;

    /// Valid tables, for the hostile-table tests.
    pub(crate) fn samples() -> Vec<Vec<u8>> {
        vec![full()]
    }

    pub(crate) fn madt(entries: &[&[u8]]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&0xFEE0_0000u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        for e in entries {
            body.extend_from_slice(e);
        }
        table(b"APIC", 7, &body)
    }

    pub(crate) fn lapic(uid: u8, id: u8, flags: u32) -> Vec<u8> {
        let mut e = vec![0, 8, uid, id];
        e.extend_from_slice(&flags.to_le_bytes());
        e
    }

    pub(crate) fn ioapic(id: u8, address: u32, base: u32) -> Vec<u8> {
        let mut e = vec![1, 12, id, 0];
        e.extend_from_slice(&address.to_le_bytes());
        e.extend_from_slice(&base.to_le_bytes());
        e
    }

    pub(crate) fn iso(source: u8, gsi: u32, flags: u16) -> Vec<u8> {
        let mut e = vec![2, 10, 0, source];
        e.extend_from_slice(&gsi.to_le_bytes());
        e.extend_from_slice(&flags.to_le_bytes());
        e
    }

    pub(crate) fn x2apic(id: u32, flags: u32, uid: u32) -> Vec<u8> {
        let mut e = vec![9, 16, 0, 0];
        e.extend_from_slice(&id.to_le_bytes());
        e.extend_from_slice(&flags.to_le_bytes());
        e.extend_from_slice(&uid.to_le_bytes());
        e
    }

    pub(crate) fn every_x86_entry() -> Vec<Vec<u8>> {
        let mut nmi_source = vec![3, 8, 0x0D, 0];
        nmi_source.extend_from_slice(&5u32.to_le_bytes());
        let lapic_nmi = vec![4, 6, 0xFF, 0x05, 0x00, 1];
        let mut address_override = vec![5, 12, 0, 0];
        address_override.extend_from_slice(&0x1_FEE0_0000u64.to_le_bytes());
        let mut x2nmi = vec![10, 12, 0x05, 0];
        x2nmi.extend_from_slice(&u32::MAX.to_le_bytes());
        x2nmi.extend_from_slice(&[1, 0, 0, 0]);
        let mut wakeup = vec![16, 24, 1, 0, 0, 0, 0, 0];
        wakeup.extend_from_slice(&0x7FF0_0000u64.to_le_bytes());
        wakeup.extend_from_slice(&0x7FF0_1000u64.to_le_bytes());
        let mut gicc = vec![11, 82];
        gicc.resize(82, 0);
        let oem = vec![0x80, 4, 0xAA, 0xBB];
        vec![
            lapic(0, 0, 1),
            lapic(1, 1, 0b10),
            ioapic(2, 0xFEC0_0000, 0),
            iso(0, 2, 0),
            iso(9, 9, 0x000D),
            nmi_source,
            lapic_nmi,
            address_override,
            x2apic(300, 1, 300),
            x2nmi,
            wakeup,
            gicc,
            oem,
        ]
    }

    fn full() -> Vec<u8> {
        let entries = every_x86_entry();
        let refs: Vec<&[u8]> = entries.iter().map(Vec::as_slice).collect();
        madt(&refs)
    }

    #[test]
    fn decodes_every_x86_structure() {
        let t = full();
        let m = Madt::parse(&t).unwrap();
        assert_eq!(m.local_apic_address, 0xFEE0_0000);
        assert!(m.pcat_compat);
        let all: Vec<Entry<'_>> = m.entries().map(Result::unwrap).collect();
        assert_eq!(all.len(), 13);
        assert_eq!(
            all[1],
            Entry::LocalApic {
                uid: 1,
                apic_id: 1,
                flags: ProcessorFlags {
                    enabled: false,
                    online_capable: true
                }
            }
        );
        assert_eq!(
            all[4],
            Entry::SourceOverride {
                source: 9,
                gsi: 9,
                flags: Inti {
                    polarity: Polarity::ActiveHigh,
                    trigger: Trigger::Level
                }
            }
        );
        assert!(matches!(
            all[6],
            Entry::LocalApicNmi {
                uid: None,
                lint: 1,
                ..
            }
        ));
        assert!(matches!(
            all[9],
            Entry::LocalX2ApicNmi {
                uid: None,
                lint: 1,
                ..
            }
        ));
        assert_eq!(
            all[10],
            Entry::MultiprocessorWakeup {
                mailbox_version: 1,
                mailbox: 0x7FF0_0000,
                reset_vector: Some(0x7FF0_1000)
            }
        );
        assert!(matches!(all[11], Entry::NotX86(Raw { kind: 11, .. })));
        assert!(matches!(all[12], Entry::Oem(Raw { kind: 0x80, .. })));
        assert_eq!(m.local_apic_base(), Ok(0x1_FEE0_0000));
        let c = census::check(&t).unwrap();
        let census::Class::Checked(s) = c.class else {
            panic!("{c:?}")
        };
        assert_eq!(s.structures(), 13);
        assert_eq!(s.of_kind(0), 2);
    }

    #[test]
    fn processor_flags_read_online_capable_only_when_disabled() {
        assert_eq!(
            ProcessorFlags::from_bits(0b11),
            ProcessorFlags {
                enabled: true,
                online_capable: false
            }
        );
        assert!(ProcessorFlags::from_bits(0).ignored());
        assert!(!ProcessorFlags::from_bits(0b10).ignored());
    }

    fn refusal(entries: &[&[u8]]) -> Error {
        census::check(&madt(entries)).unwrap_err()
    }

    #[test]
    fn reserved_and_impossible_values_are_refused_by_name() {
        // Polarity 10 and trigger 10 are reserved.
        let e = refusal(&[&iso(0, 2, 0b10)]);
        assert_eq!((e.at, e.kind), (52, ErrorKind::Value { field: "Flags" }));
        assert!(matches!(
            refusal(&[&iso(0, 2, 0b1000)]).kind,
            ErrorKind::Value { .. }
        ));
        // Only ISA can be overridden.
        let mut pci = iso(0, 2, 0);
        pci[2] = 1;
        assert_eq!(refusal(&[&pci]).kind, ErrorKind::Value { field: "Bus" });
        // LINT2 does not exist.
        assert_eq!(
            refusal(&[&[4, 6, 0, 0, 0, 2]]).kind,
            ErrorKind::Value { field: "LINT#" }
        );
        // A mailbox inside a page.
        let mut wakeup = vec![16, 24, 1, 0, 0, 0, 0, 0];
        wakeup.extend_from_slice(&0x7FF0_0008u64.to_le_bytes());
        wakeup.extend_from_slice(&0u64.to_le_bytes());
        assert_eq!(
            refusal(&[&wakeup]).kind,
            ErrorKind::Value {
                field: "MailBoxAddress"
            }
        );
    }

    #[test]
    fn structures_must_hold_their_fields() {
        for (kind, min) in [
            (0u8, 8u8),
            (1, 12),
            (2, 10),
            (3, 8),
            (4, 6),
            (5, 12),
            (9, 16),
            (10, 12),
            (16, 16),
        ] {
            let mut short = vec![kind, min - 1];
            short.resize(usize::from(min - 1), 0);
            let e = refusal(&[&short]);
            assert_eq!(
                e,
                Error::new(
                    crate::Signature::new(b"APIC"),
                    44,
                    ErrorKind::Structure {
                        kind: u16::from(kind),
                        length: usize::from(min - 1)
                    }
                ),
                "type {kind}"
            );
        }
        // Longer than the layout: accepted.
        let mut long = lapic(0, 0, 1);
        long[1] = 10;
        long.extend_from_slice(&[0, 0]);
        assert!(census::check(&madt(&[&long])).is_ok());
        // Zero, and past the end.
        assert!(matches!(
            refusal(&[&[0x80, 0]]).kind,
            ErrorKind::Structure { .. }
        ));
        assert!(matches!(
            refusal(&[&[0x80, 9, 0]]).kind,
            ErrorKind::Truncated { .. }
        ));
        // The header's fields themselves.
        let t = table(b"APIC", 7, &[0; 7]);
        assert!(matches!(
            Madt::parse(&t).unwrap_err().kind,
            ErrorKind::TooShort { .. }
        ));
    }

    #[test]
    fn what_holds_between_structures_is_checked() {
        let mut ovr = vec![5, 12, 0, 0];
        ovr.extend_from_slice(&0xFEE0_0000u64.to_le_bytes());
        assert_eq!(
            refusal(&[&ovr, &ovr]).kind,
            ErrorKind::Inconsistent {
                field: "a second local APIC address override"
            }
        );
        let a = ioapic(1, 0xFEC0_0000, 0);
        let same_address = ioapic(2, 0xFEC0_0000, 24);
        let e = refusal(&[&a, &lapic(0, 0, 1), &same_address]);
        assert_eq!(
            (e.at, e.kind),
            (
                64,
                ErrorKind::Inconsistent {
                    field: "I/O APIC address"
                }
            )
        );
        let same_base = ioapic(2, 0xFEC0_1000, 0);
        assert_eq!(
            refusal(&[&a, &same_base]).kind,
            ErrorKind::Inconsistent {
                field: "I/O APIC global system interrupt base"
            }
        );
        assert!(census::check(&madt(&[&a, &ioapic(2, 0xFEC0_1000, 24)])).is_ok());
        let many: Vec<Vec<u8>> = (0..=MAX_IO_APICS as u32)
            .map(|i| ioapic(0, 0xFEC0_0000 + i * 0x1000, i * 24))
            .collect();
        let refs: Vec<&[u8]> = many.iter().map(Vec::as_slice).collect();
        assert_eq!(
            census::check(&madt(&refs[..MAX_IO_APICS])).map(|_| ()),
            Ok(())
        );
        assert_eq!(
            refusal(&refs).kind,
            ErrorKind::Value {
                field: "more I/O APICs than 8-bit IDs tell apart"
            }
        );
    }

    #[test]
    fn interrupt_ranges_overlap_only_when_they_share_an_input() {
        assert!(!gsi_ranges_overlap(&[(0, 24), (24, 24)]));
        assert!(gsi_ranges_overlap(&[(0, 24), (23, 24)]));
        assert!(gsi_ranges_overlap(&[(48, 8), (0, 24), (50, 1)]));
        assert!(!gsi_ranges_overlap(&[(0, 0), (0, 24)]));
        assert!(!gsi_ranges_overlap(&[(u32::MAX, u32::MAX), (0, 24)]));
    }
}
