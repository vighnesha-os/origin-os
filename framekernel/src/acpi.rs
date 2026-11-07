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
/// Offset of the first SRAT resource-affinity structure (36-byte header + 12 reserved).
const SRAT_ENTRIES: usize = 48;

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
    /// An SRAT structure at this offset is truncated or has a length below 2 (or below the
    /// minimum for its type).
    SratEntry(usize),
    /// More affinity structures than the caller has room for.
    TooManyEntries,
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
    /// The ACPI power-management timer's I/O port (`PM_TMR_BLK`, offset 76), if the
    /// firmware provides one (nonzero and `PM_TMR_LEN` is 4).
    pub pm_timer_port: Option<u16>,
    /// Whether the PM timer is 32-bit (`FADT.flags` bit 8, `TMR_VAL_EXT`); else 24-bit.
    pub pm_timer_32bit: bool,
}

/// Offset of the FADT's `CENTURY` byte.
const FADT_CENTURY: usize = 108;
/// Offset of the FADT's 16-bit `IAPC_BOOT_ARCH` flags.
const FADT_IAPC_BOOT_ARCH: usize = 109;
/// Offset of `PM_TMR_BLK` (the PM timer's 32-bit I/O port address).
const FADT_PM_TMR_BLK: usize = 76;
/// Offset of `PM_TMR_LEN`.
const FADT_PM_TMR_LEN: usize = 91;
/// Offset of the `FADT.flags` dword; bit 8 is `TMR_VAL_EXT` (a 32-bit PM timer).
const FADT_FLAGS: usize = 112;
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
    let pm_tmr_blk = u32_at(fadt, FADT_PM_TMR_BLK).ok_or_else(short)?;
    let pm_tmr_len = *fadt.get(FADT_PM_TMR_LEN).ok_or_else(short)?;
    let flags = u32_at(fadt, FADT_FLAGS).ok_or_else(short)?;
    let pm_timer_port =
        (pm_tmr_len == 4 && pm_tmr_blk != 0 && pm_tmr_blk <= 0xFFFF).then_some(pm_tmr_blk as u16);
    Ok(Fadt {
        century: (0x0E..=0x7F).contains(&century).then_some(century),
        cmos_rtc_absent: arch & CMOS_RTC_NOT_PRESENT != 0,
        pm_timer_port,
        pm_timer_32bit: flags & (1 << 8) != 0,
    })
}

/// The ACPI PM timer's fixed frequency (ACPI 6.6 §4.8.3.3).
pub const PM_TIMER_HZ: u64 = 3_579_545;

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

/// One processor's NUMA affinity from the SRAT (ACPI 6.6 §5.2.16): a Processor Local APIC
/// Affinity structure (type 0) or a Processor Local x2APIC Affinity structure (type 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuAffinity {
    /// Local APIC ID (type 0) or x2APIC ID (type 2).
    pub apic_id: u32,
    /// The proximity domain (NUMA node) it belongs to.
    pub proximity_domain: u32,
    /// Usable now (flags bit 0).
    pub enabled: bool,
}

/// One memory range's NUMA affinity from the SRAT (type 1, ACPI 6.6 §5.2.16.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryAffinity {
    pub proximity_domain: u32,
    pub base: u64,
    pub length: u64,
    /// Usable now (flags bit 0).
    pub enabled: bool,
    /// Hot-pluggable (flags bit 1).
    pub hot_pluggable: bool,
    /// Non-volatile (flags bit 2).
    pub non_volatile: bool,
}

/// The 32-bit proximity domain of a type-0 structure: byte 2 is the low 8 bits, bytes 9..12
/// the high 24 (ACPI 6.6 §5.2.16.1). The caller guarantees `entry.len() >= 16`.
fn proximity_type0(entry: &[u8]) -> u32 {
    let lo = u32::from(entry.get(2).copied().unwrap_or(0));
    let hi = u32::from(entry.get(9).copied().unwrap_or(0))
        | u32::from(entry.get(10).copied().unwrap_or(0)) << 8
        | u32::from(entry.get(11).copied().unwrap_or(0)) << 16;
    lo | (hi << 8)
}

/// Lists the processor affinities in a checked SRAT (`check_table(b"SRAT")` already passed)
/// into `out`, handling type 0 (local APIC) and type 2 (x2APIC); returns how many. Other
/// structure types are skipped. A known type with a length below its minimum is refused.
pub fn srat_cpu_affinities(srat: &[u8], out: &mut [CpuAffinity]) -> Result<usize, AcpiError> {
    let mut count = 0;
    let mut at = SRAT_ENTRIES;
    while at < srat.len() {
        let (Some(&kind), Some(&len)) = (srat.get(at), srat.get(at + 1)) else {
            return Err(AcpiError::SratEntry(at));
        };
        let entry = srat
            .get(at..at + usize::from(len))
            .filter(|_| len >= 2)
            .ok_or(AcpiError::SratEntry(at))?;
        let affinity = match (kind, len) {
            (0, 16..) => Some(CpuAffinity {
                apic_id: u32::from(entry.get(3).copied().unwrap_or(0)),
                proximity_domain: proximity_type0(entry),
                enabled: u32_at(entry, 4).unwrap_or(0) & 1 != 0,
            }),
            (2, 24..) => Some(CpuAffinity {
                apic_id: u32_at(entry, 8).unwrap_or(0),
                proximity_domain: u32_at(entry, 4).unwrap_or(0),
                enabled: u32_at(entry, 12).unwrap_or(0) & 1 != 0,
            }),
            (0 | 2, _) => return Err(AcpiError::SratEntry(at)),
            _ => None,
        };
        if let Some(a) = affinity {
            *out.get_mut(count).ok_or(AcpiError::TooManyEntries)? = a;
            count += 1;
        }
        at += usize::from(len);
    }
    Ok(count)
}

/// Lists the memory affinities in a checked SRAT (type 1) into `out`; returns how many.
pub fn srat_memory_affinities(srat: &[u8], out: &mut [MemoryAffinity]) -> Result<usize, AcpiError> {
    let mut count = 0;
    let mut at = SRAT_ENTRIES;
    while at < srat.len() {
        let (Some(&kind), Some(&len)) = (srat.get(at), srat.get(at + 1)) else {
            return Err(AcpiError::SratEntry(at));
        };
        let entry = srat
            .get(at..at + usize::from(len))
            .filter(|_| len >= 2)
            .ok_or(AcpiError::SratEntry(at))?;
        if kind == 1 {
            if len < 40 {
                return Err(AcpiError::SratEntry(at));
            }
            let base = u64::from(u32_at(entry, 8).unwrap_or(0))
                | u64::from(u32_at(entry, 12).unwrap_or(0)) << 32;
            let length = u64::from(u32_at(entry, 16).unwrap_or(0))
                | u64::from(u32_at(entry, 20).unwrap_or(0)) << 32;
            let flags = u32_at(entry, 28).unwrap_or(0);
            *out.get_mut(count).ok_or(AcpiError::TooManyEntries)? = MemoryAffinity {
                proximity_domain: u32_at(entry, 2).unwrap_or(0),
                base,
                length,
                enabled: flags & 1 != 0,
                hot_pluggable: flags & 2 != 0,
                non_volatile: flags & 4 != 0,
            };
            count += 1;
        }
        at += usize::from(len);
    }
    Ok(count)
}

/// A checked SLIT (ACPI 6.6 §5.2.17): the locality count and the row-major N×N matrix of
/// relative distances. The diagonal is the normalized local distance 10; 255 means
/// unreachable.
#[derive(Clone, Copy, Debug)]
pub struct Slit<'a> {
    localities: usize,
    matrix: &'a [u8],
}

impl Slit<'_> {
    /// The number of system localities (proximity domains).
    pub fn localities(&self) -> usize {
        self.localities
    }

    /// The relative distance from locality `i` to locality `j`, or `None` if either index
    /// is out of range.
    pub fn distance(&self, i: usize, j: usize) -> Option<u8> {
        (i < self.localities && j < self.localities)
            .then(|| self.matrix.get(i * self.localities + j).copied())
            .flatten()
    }
}

/// Parses a checked SLIT (`check_table(b"SLIT")` already passed): the u64 locality count at
/// offset 36 and the following N×N distance matrix, both of which must fit in the table.
pub fn parse_slit(table: &[u8]) -> Result<Slit<'_>, AcpiError> {
    let n = usize::try_from(u64_at(table, 36).ok_or(AcpiError::Length(0))?)
        .map_err(|_| AcpiError::Length(0))?;
    let cells = n.checked_mul(n).ok_or(AcpiError::Length(0))?;
    let end = 44usize.checked_add(cells).ok_or(AcpiError::Length(0))?;
    let matrix = table
        .get(44..end)
        .ok_or(AcpiError::Length(table.len() as u32))?;
    Ok(Slit {
        localities: n,
        matrix,
    })
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
            body[FADT_PM_TMR_BLK - 36..FADT_PM_TMR_BLK - 32]
                .copy_from_slice(&0x0608u32.to_le_bytes());
            body[FADT_PM_TMR_LEN - 36] = 4;
            body[FADT_FLAGS - 36..FADT_FLAGS - 32].copy_from_slice(&(1u32 << 8).to_le_bytes());
            table(b"FACP", &body)
        };
        let qemu = fadt(0x32, 0);
        assert_eq!(check_table(&qemu, b"FACP"), Ok(()));
        let f = parse_fadt(&qemu).unwrap();
        assert_eq!((f.century, f.cmos_rtc_absent), (Some(0x32), false));
        assert_eq!(f.pm_timer_port, Some(0x0608), "PM_TMR_BLK from the fixture");
        assert!(f.pm_timer_32bit, "the fixture sets TMR_VAL_EXT");
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
        // The flags dword ends at offset 116, so a shorter table is refused.
        assert_eq!(parse_fadt(&qemu[..115]), Err(AcpiError::Length(115)));
        assert!(parse_fadt(&qemu[..116]).is_ok());
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

    /// Builds an SRAT body: 12 reserved bytes (the first dword is 1 for back-compat) then
    /// the given affinity structures, wrapped in a checked table header.
    fn srat(entries: &[&[u8]]) -> Vec<u8> {
        let mut body = vec![0u8; 12];
        body[0] = 1;
        for e in entries {
            body.extend_from_slice(e);
        }
        table(b"SRAT", &body)
    }

    /// A type-0 Processor Local APIC Affinity structure.
    fn cpu0(apic: u8, domain: u32, enabled: bool) -> Vec<u8> {
        let d = domain.to_le_bytes();
        vec![
            0,
            16,
            d[0],
            apic, // type, len, domain[7:0], apic id
            enabled as u8,
            0,
            0,
            0, // flags
            0, // local SAPIC EID
            d[1],
            d[2],
            d[3], // domain[31:8]
            0,
            0,
            0,
            0, // clock domain
        ]
    }

    /// A type-2 Processor Local x2APIC Affinity structure.
    fn cpu2(x2apic: u32, domain: u32, enabled: bool) -> Vec<u8> {
        let mut e = vec![2u8, 24, 0, 0];
        e.extend_from_slice(&domain.to_le_bytes());
        e.extend_from_slice(&x2apic.to_le_bytes());
        e.extend_from_slice(&(enabled as u32).to_le_bytes());
        e.extend_from_slice(&[0; 8]); // clock domain + reserved
        e
    }

    /// A type-1 Memory Affinity structure.
    fn mem(domain: u32, base: u64, length: u64, flags: u32) -> Vec<u8> {
        let mut e = vec![1u8, 40];
        e.extend_from_slice(&domain.to_le_bytes());
        e.extend_from_slice(&[0, 0]); // reserved
        e.extend_from_slice(&(base as u32).to_le_bytes());
        e.extend_from_slice(&((base >> 32) as u32).to_le_bytes());
        e.extend_from_slice(&(length as u32).to_le_bytes());
        e.extend_from_slice(&((length >> 32) as u32).to_le_bytes());
        e.extend_from_slice(&[0, 0, 0, 0]); // reserved
        e.extend_from_slice(&flags.to_le_bytes());
        e.extend_from_slice(&[0; 8]); // reserved
        e
    }

    #[test]
    fn srat_lists_cpu_and_memory_affinities() {
        // Two nodes: cpus 0,2 on node 0 and cpu 1 on node 1 (a 32-bit domain via the high
        // bytes), an x2APIC cpu on node 1, and a memory range per node. One extra entry of
        // an unknown type (6) must be skipped.
        let s = srat(&[
            &cpu0(0, 0, true),
            &cpu0(1, 0x0001_0000, true), // high domain bytes exercised
            &cpu0(2, 0, false),          // present but not enabled
            &cpu2(300, 0x0001_0000, true),
            &mem(0, 0, 0x1000_0000, 1), // 256 MiB, enabled
            &mem(0x0001_0000, 0x1_0000_0000, 0x1000_0000, 0b111), // enabled|hotplug|nv
            &[6u8, 4, 0, 0][..],        // unknown type, skipped
        ]);
        assert_eq!(check_table(&s, b"SRAT"), Ok(()));

        let mut cpus = [CpuAffinity::default(); 8];
        let n = srat_cpu_affinities(&s, &mut cpus).unwrap();
        assert_eq!(n, 4);
        assert_eq!(
            cpus[0],
            CpuAffinity {
                apic_id: 0,
                proximity_domain: 0,
                enabled: true
            }
        );
        assert_eq!(
            cpus[1].proximity_domain, 0x0001_0000,
            "32-bit domain decoded"
        );
        assert!(!cpus[2].enabled, "a present-but-disabled cpu");
        assert_eq!(
            cpus[3],
            CpuAffinity {
                apic_id: 300,
                proximity_domain: 0x0001_0000,
                enabled: true
            },
            "x2APIC affinity"
        );

        let mut mems = [MemoryAffinity::default(); 8];
        let n = srat_memory_affinities(&s, &mut mems).unwrap();
        assert_eq!(n, 2);
        assert_eq!(mems[0].length, 0x1000_0000);
        assert_eq!(mems[0].proximity_domain, 0);
        assert_eq!(mems[1].base, 0x1_0000_0000, "64-bit base decoded");
        assert!(mems[1].hot_pluggable && mems[1].non_volatile);

        // Distinct nodes across both kinds: {0, 0x10000} = 2.
        let mut domains = alloc::vec::Vec::new();
        for c in &cpus[..4] {
            if c.enabled && !domains.contains(&c.proximity_domain) {
                domains.push(c.proximity_domain);
            }
        }
        assert_eq!(domains.len(), 2);
    }

    #[test]
    fn srat_rejects_malformed_and_overflow() {
        // A type-0 structure too short for its fixed 16 bytes is refused at its offset.
        let s = srat(&[&cpu0(0, 0, true), &[0u8, 8, 0, 0, 0, 0, 0, 0][..]]);
        let mut cpus = [CpuAffinity::default(); 8];
        assert_eq!(
            srat_cpu_affinities(&s, &mut cpus),
            Err(AcpiError::SratEntry(SRAT_ENTRIES + 16))
        );

        // A structure whose length runs past the table.
        let s = srat(&[&[0u8, 200, 0, 0][..]]);
        assert_eq!(
            srat_cpu_affinities(&s, &mut cpus),
            Err(AcpiError::SratEntry(SRAT_ENTRIES))
        );

        // More enabled cpus than the caller's buffer holds.
        let s = srat(&[&cpu0(0, 0, true), &cpu0(1, 0, true), &cpu0(2, 0, true)]);
        let mut small = [CpuAffinity::default(); 2];
        assert_eq!(
            srat_cpu_affinities(&s, &mut small),
            Err(AcpiError::TooManyEntries)
        );

        // A type-1 memory structure below 40 bytes is refused.
        let s = srat(&[&[1u8, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..]]);
        let mut mems = [MemoryAffinity::default(); 4];
        assert_eq!(
            srat_memory_affinities(&s, &mut mems),
            Err(AcpiError::SratEntry(SRAT_ENTRIES))
        );
    }

    /// A SLIT table: an 8-byte locality count then the row-major distance matrix.
    fn slit(n: u64, matrix: &[u8]) -> Vec<u8> {
        let mut body = n.to_le_bytes().to_vec();
        body.extend_from_slice(matrix);
        table(b"SLIT", &body)
    }

    #[test]
    fn slit_distance_matrix() {
        // Two localities, 10 local and 21 across — the usual single-hop ratio.
        let s = slit(2, &[10, 21, 21, 10]);
        assert_eq!(check_table(&s, b"SLIT"), Ok(()));
        let t = parse_slit(&s).unwrap();
        assert_eq!(t.localities(), 2);
        assert_eq!(t.distance(0, 0), Some(10));
        assert_eq!(t.distance(0, 1), Some(21));
        assert_eq!(t.distance(1, 0), Some(21));
        assert_eq!(t.distance(1, 1), Some(10));
        assert_eq!(t.distance(2, 0), None, "out of range");
        assert_eq!(t.distance(0, 2), None);

        // A 3-locality asymmetric matrix round-trips cell for cell.
        let m = [10, 16, 32, 16, 10, 20, 32, 20, 10];
        let s = slit(3, &m);
        let t = parse_slit(&s).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                assert_eq!(t.distance(i, j), Some(m[i * 3 + j]));
            }
        }
    }

    #[test]
    fn srat_and_slit_mutations_never_panic() {
        // Valid SRAT and SLIT tables, then thousands of mutated copies through the parsers:
        // nothing may panic, and a mutated locality count can never drive a huge allocation
        // (CLAUDE.md §72).
        let base_srat = srat(&[
            &cpu0(0, 0, true),
            &cpu0(1, 1, true),
            &cpu2(300, 1, true),
            &mem(0, 0, 0x1000_0000, 1),
            &mem(1, 0x1_0000_0000, 0x1000_0000, 0b111),
        ]);
        let base_slit = slit(2, &[10, 21, 21, 10]);

        let mut rng = crate::testing::Rng::new(0x5247_5A54);
        for _ in 0..crate::testing::cases(10_000) {
            for base in [&base_srat, &base_slit] {
                let mut data = base.clone();
                crate::testing::mutate(&mut data, &mut rng);
                // Direct calls (the parsers assume a checked table, but must still be
                // panic-safe on arbitrary bytes).
                let mut cpus = [CpuAffinity::default(); 16];
                let _ = srat_cpu_affinities(&data, &mut cpus);
                let mut mems = [MemoryAffinity::default(); 16];
                let _ = srat_memory_affinities(&data, &mut mems);
                if let Ok(s) = parse_slit(&data) {
                    let n = s.localities().min(8);
                    for i in 0..n {
                        for j in 0..n {
                            let _ = s.distance(i, j);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn slit_rejects_a_truncated_matrix() {
        // Claims 4 localities (16 cells) but carries only 4 bytes of matrix.
        let s = slit(4, &[10, 21, 21, 10]);
        assert!(matches!(parse_slit(&s), Err(AcpiError::Length(_))));
        // A locality count that would overflow the table length is refused, not panicked.
        let s = slit(u64::MAX, &[10]);
        assert!(matches!(parse_slit(&s), Err(AcpiError::Length(_))));
    }
}
