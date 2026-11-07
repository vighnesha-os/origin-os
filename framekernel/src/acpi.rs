//! ACPI table parsing for processor discovery (M3; CLAUDE.md §24 SMP start-up and CPU
//! topology).
//!
//! References: ACPI 6.6 §5.2.5 (RSDP), §5.2.6 (system description table header), §5.2.8
//! (XSDT), §5.2.9 (FADT: the RTC century register and the boot-architecture flags) and
//! §5.2.12 (MADT, with the Processor Local APIC structure, type 0, and the Processor Local
//! x2APIC structure, type 9).
//!
//! Firmware tables are untrusted input. Every function here parses a byte slice the caller
//! copied or mapped, checks signatures, lengths and checksums before using a field, never
//! panics, and needs no `unsafe`.

/// Bytes of an ACPI 2.0+ RSDP that are checksummed and parsed.
pub const RSDP_LENGTH: usize = 36;
/// Bytes of a system description table header.
pub const HEADER_LENGTH: usize = 36;
/// Offset of the first MADT interrupt controller structure.
const MADT_ENTRIES: usize = 44;

/// Why a table was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpiError {
    /// The RSDP signature is not `"RSD PTR "`.
    RsdpSignature,
    /// The RSDP's 20-byte or extended checksum is wrong.
    RsdpChecksum,
    /// The RSDP is revision 0 (ACPI 1.0, no XSDT), which Origin does not support.
    RsdpRevision(u8),
    /// The RSDP's length is below 36 bytes.
    RsdpLength(u32),
    /// A table's signature is not the one expected.
    Signature([u8; 4]),
    /// A table's length is below its header or does not match the bytes given.
    Length(u32),
    /// A table's checksum is wrong.
    Checksum([u8; 4]),
    /// A MADT structure at this offset is truncated or has a length below 2.
    MadtEntry(usize),
    /// More processors than the caller has room for.
    TooManyProcessors,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let bytes = b.get(at..at.checked_add(4)?)?;
    let mut v = [0u8; 4];
    v.copy_from_slice(bytes);
    Some(u32::from_le_bytes(v))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    let bytes = b.get(at..at.checked_add(8)?)?;
    let mut v = [0u8; 8];
    v.copy_from_slice(bytes);
    Some(u64::from_le_bytes(v))
}

/// The ACPI checksum rule: all bytes sum to zero modulo 256.
fn sums_to_zero(b: &[u8]) -> bool {
    b.iter().fold(0u8, |sum, &x| sum.wrapping_add(x)) == 0
}

/// What the RSDP says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rsdp {
    /// ACPI revision field (2 or more).
    pub revision: u8,
    /// Physical address of the XSDT.
    pub xsdt: u64,
}

/// Parses the first [`RSDP_LENGTH`] bytes of an RSDP.
pub fn parse_rsdp(b: &[u8]) -> Result<Rsdp, AcpiError> {
    let b = b.get(..RSDP_LENGTH).ok_or(AcpiError::RsdpLength(0))?;
    if b.get(..8) != Some(b"RSD PTR ") {
        return Err(AcpiError::RsdpSignature);
    }
    if !b.get(..20).is_some_and(sums_to_zero) {
        return Err(AcpiError::RsdpChecksum);
    }
    let revision = b.get(15).copied().unwrap_or(0);
    if revision < 2 {
        return Err(AcpiError::RsdpRevision(revision));
    }
    let length = u32_at(b, 20).unwrap_or(0);
    if (length as usize) < RSDP_LENGTH {
        return Err(AcpiError::RsdpLength(length));
    }
    // The extended checksum covers `length` bytes; the 36 defined ones are what Origin
    // reads, so a longer future RSDP is checked over those.
    if !sums_to_zero(b) {
        return Err(AcpiError::RsdpChecksum);
    }
    Ok(Rsdp {
        revision,
        xsdt: u64_at(b, 24).unwrap_or(0),
    })
}

/// The `length` field of a table header, so the caller knows how much to read.
pub fn table_length(header: &[u8]) -> Result<u32, AcpiError> {
    let length = u32_at(header, 4).ok_or(AcpiError::Length(0))?;
    if (length as usize) < HEADER_LENGTH {
        return Err(AcpiError::Length(length));
    }
    Ok(length)
}

/// The signature of a table header.
pub fn signature(header: &[u8]) -> Option<[u8; 4]> {
    let mut s = [0u8; 4];
    s.copy_from_slice(header.get(..4)?);
    Some(s)
}

/// Checks a whole table: `expected` signature, a length equal to `table.len()`, and its
/// checksum.
pub fn check_table(table: &[u8], expected: &[u8; 4]) -> Result<(), AcpiError> {
    let sig = signature(table).ok_or(AcpiError::Length(0))?;
    if &sig != expected {
        return Err(AcpiError::Signature(sig));
    }
    let length = table_length(table)?;
    if length as usize != table.len() {
        return Err(AcpiError::Length(length));
    }
    if !sums_to_zero(table) {
        return Err(AcpiError::Checksum(sig));
    }
    Ok(())
}

/// The table addresses in a checked XSDT.
pub fn xsdt_entries(xsdt: &[u8]) -> impl Iterator<Item = u64> + '_ {
    xsdt.get(HEADER_LENGTH..)
        .unwrap_or(&[])
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
}

/// What the kernel reads from the FADT (signature `"FACP"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fadt {
    /// The CMOS index of the RTC's century register, if the firmware names a usable one
    /// (`CENTURY`, offset 108; 0 means none). Indexes 00h–0Dh are the clock's own
    /// registers and 80h and above are outside the standard bank, so neither is accepted.
    pub century: Option<u8>,
    /// `IAPC_BOOT_ARCH` (offset 109) bit 5: the platform has no CMOS RTC.
    pub cmos_rtc_absent: bool,
}

/// Offset of the FADT's `CENTURY` byte.
const FADT_CENTURY: usize = 108;
/// Offset of the FADT's 16-bit `IAPC_BOOT_ARCH` flags.
const FADT_IAPC_BOOT_ARCH: usize = 109;
/// `IAPC_BOOT_ARCH` bit 5: CMOS RTC not present.
const CMOS_RTC_NOT_PRESENT: u16 = 1 << 5;

/// Reads the fields of a checked FADT that the kernel uses. A table too short to hold
/// them (an ACPI 1.0 FADT has them; anything shorter is malformed) is refused.
pub fn parse_fadt(fadt: &[u8]) -> Result<Fadt, AcpiError> {
    let short = || AcpiError::Length(u32::try_from(fadt.len()).unwrap_or(u32::MAX));
    let century = *fadt.get(FADT_CENTURY).ok_or_else(short)?;
    let arch = fadt
        .get(FADT_IAPC_BOOT_ARCH..FADT_IAPC_BOOT_ARCH + 2)
        .ok_or_else(short)?;
    let arch = u16::from_le_bytes([
        arch.first().copied().unwrap_or(0),
        arch.get(1).copied().unwrap_or(0),
    ]);
    Ok(Fadt {
        century: (0x0E..=0x7F).contains(&century).then_some(century),
        cmos_rtc_absent: arch & CMOS_RTC_NOT_PRESENT != 0,
    })
}

/// One processor the MADT lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Processor {
    /// Its local APIC ID (x2APIC ID for type 9 structures).
    pub apic_id: u32,
    /// Its ACPI processor UID.
    pub uid: u32,
    /// Usable now (flags bit 0).
    pub enabled: bool,
    /// Can be brought online later (flags bit 1); meaningless when `enabled`.
    pub online_capable: bool,
}

/// Lists the processors in a checked MADT into `out` and returns how many there are. A
/// processor listed twice (as type 0 and type 9) is kept once. Other structure types are
/// skipped.
pub fn madt_processors(madt: &[u8], out: &mut [Processor]) -> Result<usize, AcpiError> {
    let mut count = 0;
    let mut at = MADT_ENTRIES;
    while at < madt.len() {
        let (Some(&kind), Some(&len)) = (madt.get(at), madt.get(at + 1)) else {
            return Err(AcpiError::MadtEntry(at));
        };
        let entry = madt
            .get(at..at + usize::from(len))
            .filter(|_| len >= 2)
            .ok_or(AcpiError::MadtEntry(at))?;
        let processor = match (kind, len) {
            (0, 8..) => u32_at(entry, 4).map(|flags| Processor {
                apic_id: u32::from(entry.get(3).copied().unwrap_or(0)),
                uid: u32::from(entry.get(2).copied().unwrap_or(0)),
                enabled: flags & 1 != 0,
                online_capable: flags & 2 != 0,
            }),
            (9, 16..) => match (u32_at(entry, 4), u32_at(entry, 8), u32_at(entry, 12)) {
                (Some(id), Some(flags), Some(uid)) => Some(Processor {
                    apic_id: id,
                    uid,
                    enabled: flags & 1 != 0,
                    online_capable: flags & 2 != 0,
                }),
                _ => None,
            },
            (0 | 9, _) => return Err(AcpiError::MadtEntry(at)),
            _ => None,
        };
        if let Some(p) = processor {
            let known = out
                .get(..count)
                .is_some_and(|seen| seen.iter().any(|q| q.apic_id == p.apic_id));
            if !known {
                *out.get_mut(count).ok_or(AcpiError::TooManyProcessors)? = p;
                count += 1;
            }
        }
        at += usize::from(len);
    }
    Ok(count)
}

/// An I/O APIC the MADT lists (structure type 1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IoApic {
    pub id: u8,
    /// MMIO base of its registers.
    pub address: u32,
    /// The first global system interrupt this I/O APIC handles.
    pub gsi_base: u32,
}

/// An interrupt source override (structure type 2): a legacy ISA IRQ is delivered as a
/// different global system interrupt, with a polarity/trigger override.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceOverride {
    pub source_irq: u8,
    pub gsi: u32,
    pub flags: u16,
}

/// Lists the I/O APICs in a checked MADT into `out`; returns how many.
pub fn madt_ioapics(madt: &[u8], out: &mut [IoApic]) -> Result<usize, AcpiError> {
    madt_scan(
        madt,
        |kind, entry, count, out: &mut [IoApic]| {
            if kind == 1 && entry.len() >= 12 {
                let ioapic = IoApic {
                    id: entry.get(2).copied().unwrap_or(0),
                    address: u32_at(entry, 4).unwrap_or(0),
                    gsi_base: u32_at(entry, 8).unwrap_or(0),
                };
                *out.get_mut(count).ok_or(AcpiError::TooManyProcessors)? = ioapic;
                return Ok(true);
            }
            Ok(false)
        },
        out,
    )
}

/// Lists the interrupt source overrides into `out`; returns how many.
pub fn madt_overrides(madt: &[u8], out: &mut [SourceOverride]) -> Result<usize, AcpiError> {
    madt_scan(
        madt,
        |kind, entry, count, out: &mut [SourceOverride]| {
            if kind == 2 && entry.len() >= 10 {
                let iso = SourceOverride {
                    source_irq: entry.get(3).copied().unwrap_or(0),
                    gsi: u32_at(entry, 4).unwrap_or(0),
                    flags: u16::from_le_bytes([
                        entry.get(8).copied().unwrap_or(0),
                        entry.get(9).copied().unwrap_or(0),
                    ]),
                };
                *out.get_mut(count).ok_or(AcpiError::TooManyProcessors)? = iso;
                return Ok(true);
            }
            Ok(false)
        },
        out,
    )
}

/// The global system interrupt and flags a legacy ISA `irq` maps to: the matching override,
/// else the identity mapping with default (active-high, edge) flags.
pub fn gsi_for_irq(irq: u8, overrides: &[SourceOverride]) -> (u32, u16) {
    overrides
        .iter()
        .find(|o| o.source_irq == irq)
        .map_or((u32::from(irq), 0), |o| (o.gsi, o.flags))
}

/// Walks a checked MADT, calling `f(kind, entry, count_so_far, out)` for each structure;
/// `f` returns whether it recorded an entry. Returns how many were recorded.
fn madt_scan<T>(
    madt: &[u8],
    mut f: impl FnMut(u8, &[u8], usize, &mut [T]) -> Result<bool, AcpiError>,
    out: &mut [T],
) -> Result<usize, AcpiError> {
    let mut count = 0;
    let mut at = MADT_ENTRIES;
    while at < madt.len() {
        let (Some(&kind), Some(&len)) = (madt.get(at), madt.get(at + 1)) else {
            return Err(AcpiError::MadtEntry(at));
        };
        let entry = madt
            .get(at..at + usize::from(len))
            .filter(|_| len >= 2)
            .ok_or(AcpiError::MadtEntry(at))?;
        if f(kind, entry, count, out)? {
            count += 1;
        }
        at += usize::from(len);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix_checksum(b: &mut [u8], at: usize) {
        b[at] = 0;
        let sum = b.iter().fold(0u8, |s, &x| s.wrapping_add(x));
        b[at] = 0u8.wrapping_sub(sum);
    }

    fn rsdp(xsdt: u64) -> Vec<u8> {
        let mut b = vec![0u8; 36];
        b[..8].copy_from_slice(b"RSD PTR ");
        b[9..15].copy_from_slice(b"ORIGIN");
        b[15] = 2;
        b[20..24].copy_from_slice(&36u32.to_le_bytes());
        b[24..32].copy_from_slice(&xsdt.to_le_bytes());
        fix_checksum(&mut b[..20], 8);
        fix_checksum(&mut b, 32);
        b
    }

    fn table(sig: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 36];
        b[..4].copy_from_slice(sig);
        b.extend_from_slice(body);
        let len = b.len() as u32;
        b[4..8].copy_from_slice(&len.to_le_bytes());
        b[8] = 1;
        fix_checksum(&mut b, 9);
        b
    }

    fn madt(entries: &[&[u8]]) -> Vec<u8> {
        let mut body = vec![0u8; 8]; // local APIC address, flags
        body[..4].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
        for e in entries {
            body.extend_from_slice(e);
        }
        table(b"APIC", &body)
    }

    fn lapic(uid: u8, id: u8, flags: u32) -> Vec<u8> {
        let mut e = vec![0, 8, uid, id];
        e.extend_from_slice(&flags.to_le_bytes());
        e
    }

    fn x2apic(id: u32, flags: u32, uid: u32) -> Vec<u8> {
        let mut e = vec![9, 16, 0, 0];
        e.extend_from_slice(&id.to_le_bytes());
        e.extend_from_slice(&flags.to_le_bytes());
        e.extend_from_slice(&uid.to_le_bytes());
        e
    }

    #[test]
    fn rsdp_is_checked_strictly() {
        let good = rsdp(0x1FB7_D0E8);
        assert_eq!(
            parse_rsdp(&good),
            Ok(Rsdp {
                revision: 2,
                xsdt: 0x1FB7_D0E8
            })
        );
        let mut b = good.clone();
        b[0] = b'X';
        assert_eq!(parse_rsdp(&b), Err(AcpiError::RsdpSignature));
        let mut b = good.clone();
        b[10] ^= 1; // inside the first 20 bytes
        assert_eq!(parse_rsdp(&b), Err(AcpiError::RsdpChecksum));
        let mut b = good.clone();
        b[25] ^= 1; // only the extended checksum covers this
        assert_eq!(parse_rsdp(&b), Err(AcpiError::RsdpChecksum));
        let mut b = good.clone();
        b[15] = 0;
        fix_checksum(&mut b[..20], 8);
        assert_eq!(parse_rsdp(&b), Err(AcpiError::RsdpRevision(0)));
        assert_eq!(parse_rsdp(&good[..35]), Err(AcpiError::RsdpLength(0)));
    }

    #[test]
    fn tables_are_checked_and_the_xsdt_lists_addresses() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x1000u64.to_le_bytes());
        body.extend_from_slice(&0x2000u64.to_le_bytes());
        let xsdt = table(b"XSDT", &body);
        assert_eq!(check_table(&xsdt, b"XSDT"), Ok(()));
        assert_eq!(xsdt_entries(&xsdt).collect::<Vec<_>>(), [0x1000, 0x2000]);
        assert_eq!(
            check_table(&xsdt, b"APIC"),
            Err(AcpiError::Signature(*b"XSDT"))
        );
        assert_eq!(
            check_table(&xsdt[..50], b"XSDT"),
            Err(AcpiError::Length(52))
        );
        let mut bad = xsdt.clone();
        bad[40] ^= 0x80;
        assert_eq!(
            check_table(&bad, b"XSDT"),
            Err(AcpiError::Checksum(*b"XSDT"))
        );
        let mut short = xsdt.clone();
        short[4..8].copy_from_slice(&20u32.to_le_bytes());
        assert_eq!(table_length(&short), Err(AcpiError::Length(20)));
    }

    #[test]
    fn madt_lists_processors_of_both_kinds_once() {
        let unknown = [1u8, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // an I/O APIC: skipped
        let m = madt(&[
            &lapic(0, 0, 1),
            &unknown,
            &lapic(1, 1, 1),
            &lapic(2, 2, 0b10), // not enabled, online capable
            &lapic(3, 3, 0),    // neither
            &x2apic(1, 1, 1),   // the same CPU again
            &x2apic(300, 1, 7),
        ]);
        assert_eq!(check_table(&m, b"APIC"), Ok(()));
        let mut out = [Processor::default(); 8];
        let n = madt_processors(&m, &mut out).unwrap();
        assert_eq!(n, 5);
        let ids: Vec<_> = out[..n].iter().map(|p| (p.apic_id, p.enabled)).collect();
        assert_eq!(
            ids,
            [(0, true), (1, true), (2, false), (3, false), (300, true)]
        );
        assert!(out[2].online_capable && !out[3].online_capable);
        assert_eq!(out[4].uid, 7);
        let mut small = [Processor::default(); 2];
        assert_eq!(
            madt_processors(&m, &mut small),
            Err(AcpiError::TooManyProcessors)
        );
    }

    #[test]
    fn fadt_century_register_and_rtc_presence() {
        // An ACPI 6 FADT is 276 bytes: a 240-byte body after the header.
        let fadt = |century: u8, arch: u16| {
            let mut body = vec![0u8; 240];
            body[FADT_CENTURY - 36] = century;
            body[FADT_IAPC_BOOT_ARCH - 36..FADT_IAPC_BOOT_ARCH - 34]
                .copy_from_slice(&arch.to_le_bytes());
            table(b"FACP", &body)
        };
        let qemu = fadt(0x32, 0);
        assert_eq!(check_table(&qemu, b"FACP"), Ok(()));
        assert_eq!(
            parse_fadt(&qemu),
            Ok(Fadt {
                century: Some(0x32),
                cmos_rtc_absent: false
            })
        );
        for unusable in [0x00, 0x0B, 0x0D, 0x80, 0xFF] {
            assert_eq!(
                parse_fadt(&fadt(unusable, 0)).unwrap().century,
                None,
                "{unusable:#x}"
            );
        }
        assert_eq!(parse_fadt(&fadt(0x0E, 0)).unwrap().century, Some(0x0E));
        assert!(parse_fadt(&fadt(0x32, 1 << 5)).unwrap().cmos_rtc_absent);
        assert!(!parse_fadt(&fadt(0x32, !(1 << 5))).unwrap().cmos_rtc_absent);
        assert_eq!(parse_fadt(&qemu[..110]), Err(AcpiError::Length(110)));
        assert!(parse_fadt(&qemu[..111]).is_ok());
    }

    /// Random firmware tables never panic the parsers, and anything accepted is
    /// self-consistent: checksums sum to zero, MADT processors are listed once each, a
    /// century register is in the standard bank.
    #[test]
    fn mutations_never_panic_and_accepted_tables_are_consistent() {
        use crate::testing::{Rng, mutate};
        let mut rng = Rng::new(0xAC91);
        let good_rsdp = rsdp(0x1FB7_D0E8);
        let good_madt = madt(&[
            &lapic(0, 0, 1),
            &lapic(1, 1, 1),
            &x2apic(300, 1, 7),
            &[1, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ]);
        let mut fadt_body = vec![0u8; 240];
        fadt_body[FADT_CENTURY - 36] = 0x32;
        let good_fadt = table(b"FACP", &fadt_body);
        let mut accepted = [0u32; 3];
        for case in 0..crate::testing::cases(6000) {
            let mut r = good_rsdp.clone();
            mutate(&mut r, &mut rng);
            if let Ok(p) = parse_rsdp(&r) {
                assert!(p.revision >= 2, "case {case}");
                assert!(
                    sums_to_zero(&r[..20]) && sums_to_zero(&r[..36]),
                    "case {case}"
                );
                accepted[0] += 1;
            }
            let mut m = good_madt.clone();
            mutate(&mut m, &mut rng);
            if case % 2 == 0 && m.len() >= 36 {
                let len = m.len() as u32;
                m[4..8].copy_from_slice(&len.to_le_bytes());
                fix_checksum(&mut m, 9);
            }
            if check_table(&m, b"APIC").is_ok() {
                assert!(sums_to_zero(&m), "case {case}");
                let mut out = [Processor::default(); 8];
                if let Ok(n) = madt_processors(&m, &mut out) {
                    accepted[1] += 1;
                    for (i, p) in out[..n].iter().enumerate() {
                        assert!(
                            out[i + 1..n].iter().all(|q| q.apic_id != p.apic_id),
                            "case {case}"
                        );
                    }
                }
            }
            let mut f = good_fadt.clone();
            mutate(&mut f, &mut rng);
            if let Ok(fadt) = parse_fadt(&f) {
                assert!(
                    fadt.century.is_none_or(|c| (0x0E..=0x7F).contains(&c)),
                    "case {case}"
                );
                accepted[2] += 1;
            }
        }
        assert!(accepted.iter().all(|&n| n > 100), "{accepted:?}");
    }

    fn ioapic(id: u8, addr: u32, gsi: u32) -> Vec<u8> {
        let mut e = alloc::vec![1u8, 12, id, 0];
        e.extend_from_slice(&addr.to_le_bytes());
        e.extend_from_slice(&gsi.to_le_bytes());
        e
    }

    fn iso(source: u8, gsi: u32, flags: u16) -> Vec<u8> {
        let mut e = alloc::vec![2u8, 10, 0, source];
        e.extend_from_slice(&gsi.to_le_bytes());
        e.extend_from_slice(&flags.to_le_bytes());
        e
    }

    #[test]
    fn ioapics_overrides_and_gsi_mapping() {
        let m = madt(&[
            &lapic(0, 0, 1),
            &ioapic(2, 0xFEC0_0000, 0),
            &iso(0, 2, 0),      // the PIT's IRQ0 is delivered as GSI 2
            &iso(9, 9, 0x000D), // active-low, level-triggered
        ]);
        assert_eq!(check_table(&m, b"APIC"), Ok(()));
        let mut ios = [IoApic::default(); 4];
        assert_eq!(madt_ioapics(&m, &mut ios), Ok(1));
        assert_eq!(
            ios[0],
            IoApic {
                id: 2,
                address: 0xFEC0_0000,
                gsi_base: 0
            }
        );
        let mut over = [SourceOverride::default(); 4];
        assert_eq!(madt_overrides(&m, &mut over), Ok(2));
        assert_eq!(gsi_for_irq(0, &over[..2]), (2, 0), "IRQ0 -> GSI 2");
        assert_eq!(gsi_for_irq(9, &over[..2]), (9, 0x000D));
        assert_eq!(
            gsi_for_irq(1, &over[..2]),
            (1, 0),
            "no override: identity, default flags"
        );
    }

    #[test]
    fn malformed_madt_structures_are_refused() {
        for bad in [
            vec![0u8, 0],                                // length 0
            vec![0u8, 1],                                // length 1
            vec![0u8, 6, 0, 0, 0, 0], // a type-0 structure too short for its flags
            vec![9u8, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], // a type-9 structure too short
            vec![0u8, 200, 0, 0],     // runs past the table
        ] {
            let m = madt(&[&lapic(0, 0, 1), &bad]);
            let mut out = [Processor::default(); 4];
            assert_eq!(
                madt_processors(&m, &mut out),
                Err(AcpiError::MadtEntry(52)),
                "{bad:?}"
            );
        }
        // A lone trailing byte cannot hold a structure header.
        let mut m = madt(&[&lapic(0, 0, 1)]);
        m.push(0);
        let mut out = [Processor::default(); 4];
        assert_eq!(madt_processors(&m, &mut out), Err(AcpiError::MadtEntry(52)));
    }
}
