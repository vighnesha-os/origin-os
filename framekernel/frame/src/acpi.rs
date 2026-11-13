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
#![forbid(unsafe_code)]

/// Bytes of an ACPI 2.0+ RSDP that are checksummed and parsed.
pub const RSDP_LENGTH: usize = 36;
/// Bytes of a system description table header.
pub const HEADER_LENGTH: usize = 36;
/// Offset of a table header's revision byte.
pub const HEADER_REVISION: usize = 8;
/// Offset of the first MADT interrupt controller structure.
const MADT_ENTRIES: usize = 44;
/// The first MADT revision whose Local APIC flags define bit 1, Online Capable: ACPI 6.3 (ECR
/// 1948), MADT revision 5; ACPI 6.6's MADT is revision 7 (§5.2.12, §5.2.12.2). Below it the bit is
/// reserved, and is not read (ADR-0008 Amendment 1, D2; research ledger
/// `docs/research/2026-11-13-any-processor-count.md`, §3a).
pub const MADT_ONLINE_CAPABLE_REVISION: u8 = 5;
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

/// The longest table the kernel will map and read. The length field is firmware data read
/// before the checksum can be checked; a valid 32-bit length would otherwise map and walk up
/// to 4 GiB. The largest tables it reads (an SRAT or MADT for a very large machine) are
/// well under a megabyte, so this is deliberately generous.
pub const MAX_TABLE_LENGTH: u32 = 16 << 20;

/// The `length` field of a table header, so the caller knows how much to read.
pub fn table_length(header: &[u8]) -> Result<u32, AcpiError> {
    let length = u32_at(header, 4).ok_or(AcpiError::Length(0))?;
    if (length as usize) < HEADER_LENGTH || length > MAX_TABLE_LENGTH {
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
    /// Whether the firmware says the platform has an 8042 keyboard controller
    /// (`IAPC_BOOT_ARCH` bit 1), or `None` if the table predates that flag (FADT revision 1,
    /// ACPI 1.0, where the byte is reserved).
    pub i8042: Option<bool>,
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
/// `IAPC_BOOT_ARCH` bit 1: the platform has an 8042 or equivalent (ACPI 2.0 and later).
const HAS_8042: u16 = 1 << 1;
/// Offset of the FADT's revision in its table header.
const FADT_REVISION: usize = 8;
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
    let revision = *fadt.get(FADT_REVISION).ok_or_else(short)?;
    let pm_timer_port =
        (pm_tmr_len == 4 && pm_tmr_blk != 0 && pm_tmr_blk <= 0xFFFF).then_some(pm_tmr_blk as u16);
    Ok(Fadt {
        century: (0x0E..=0x7F).contains(&century).then_some(century),
        cmos_rtc_absent: arch & CMOS_RTC_NOT_PRESENT != 0,
        i8042: (revision >= 2).then_some(arch & HAS_8042 != 0),
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
    /// Can be enabled while the system runs: flags bit 1 with bit 0 clear, read only from MADT
    /// revision [`MADT_ONLINE_CAPABLE_REVISION`] on. Never set together with `enabled`, where
    /// the bit is reserved.
    pub online_capable: bool,
}

/// Lists the processors in a checked MADT into `out` and returns how many there are. A
/// processor listed twice (as type 0 and type 9) is kept once. Other structure types are
/// skipped.
pub fn madt_processors(madt: &[u8], out: &mut [Processor]) -> Result<usize, AcpiError> {
    let reads_online_capable = madt
        .get(HEADER_REVISION)
        .is_some_and(|&revision| revision >= MADT_ONLINE_CAPABLE_REVISION);
    let online_capable = |flags: u32| reads_online_capable && flags & 1 == 0 && flags & 2 != 0;
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
                online_capable: online_capable(flags),
            }),
            (9, 16..) => match (u32_at(entry, 4), u32_at(entry, 8), u32_at(entry, 12)) {
                (Some(id), Some(flags), Some(uid)) => Some(Processor {
                    apic_id: id,
                    uid,
                    enabled: flags & 1 != 0,
                    online_capable: online_capable(flags),
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

    /// A MADT of revision 6, ACPI 6.5's.
    fn madt(entries: &[&[u8]]) -> Vec<u8> {
        madt_of(6, entries)
    }

    fn madt_of(revision: u8, entries: &[&[u8]]) -> Vec<u8> {
        let mut body = vec![0u8; 8]; // local APIC address, flags
        body[..4].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
        for e in entries {
            body.extend_from_slice(e);
        }
        let mut t = table(b"APIC", &body);
        t[HEADER_REVISION] = revision;
        fix_checksum(&mut t, 9);
        t
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

    /// ADR-0008 Amendment 1, D2: Online Capable is read from MADT revision 5 on, and only with
    /// Enabled clear; below revision 5, and beside Enabled, the bit is reserved.
    #[test]
    fn online_capable_is_read_from_madt_revision_5_and_only_without_enabled() {
        let entries: [&[u8]; 4] = [
            &lapic(0, 0, 1),
            &lapic(1, 1, 0b11),    // enabled: bit 1 reserved
            &lapic(2, 2, 0b10),    // online capable
            &x2apic(400, 0b10, 9), // online capable, above 255
        ];
        for (revision, read) in [(1, false), (4, false), (5, true), (6, true), (255, true)] {
            let m = madt_of(revision, &entries);
            assert_eq!(check_table(&m, b"APIC"), Ok(()));
            let mut out = [Processor::default(); 4];
            assert_eq!(madt_processors(&m, &mut out), Ok(4));
            let capable: Vec<_> = out.iter().map(|p| p.online_capable).collect();
            assert_eq!(capable, [false, false, read, read], "revision {revision}");
            assert!(out[1].enabled && !out[2].enabled && !out[3].enabled);
        }
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

    #[test]
    fn the_fadt_says_whether_there_is_an_8042_from_revision_2() {
        let fadt = |revision: u8, arch: u16| {
            let mut body = vec![0u8; 240];
            body[FADT_IAPC_BOOT_ARCH - 36..FADT_IAPC_BOOT_ARCH - 34]
                .copy_from_slice(&arch.to_le_bytes());
            let mut t = table(b"FACP", &body);
            t[FADT_REVISION] = revision;
            t
        };
        // ACPI 1.0's table has no such flag: the byte is reserved, so zero means nothing.
        assert_eq!(parse_fadt(&fadt(1, 0)).unwrap().i8042, None);
        assert_eq!(parse_fadt(&fadt(1, 1 << 1)).unwrap().i8042, None);
        for revision in [2, 3, 5, 6] {
            assert_eq!(
                parse_fadt(&fadt(revision, 1 << 1)).unwrap().i8042,
                Some(true)
            );
            assert_eq!(parse_fadt(&fadt(revision, 0)).unwrap().i8042, Some(false));
            assert_eq!(
                parse_fadt(&fadt(revision, !(1 << 1))).unwrap().i8042,
                Some(false),
                "only bit 1 decides"
            );
        }
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

    /// An independent walk of a table's `(kind, length)` structures from `start`: each one's
    /// `(offset, kind, length)`, or `None` if a header is cut off, a length is below 2 or a
    /// structure runs past the table. The soaks compare the parsers against it.
    fn walk(table: &[u8], start: usize) -> Option<Vec<(usize, u8, usize)>> {
        let mut at = start;
        let mut out = Vec::new();
        while at < table.len() {
            let (kind, len) = (*table.get(at)?, usize::from(*table.get(at + 1)?));
            if len < 2 || at + len > table.len() {
                return None;
            }
            out.push((at, kind, len));
            at += len;
        }
        Some(out)
    }

    fn le32(b: &[u8]) -> u32 {
        u32::from_le_bytes(b[..4].try_into().unwrap())
    }

    /// Restores a mutated table's length field and checksum, so what lies behind the
    /// signature, length and checksum checks is reached.
    fn reseal(t: &mut [u8]) {
        if t.len() >= 36 {
            let len = t.len() as u32;
            t[4..8].copy_from_slice(&len.to_le_bytes());
            fix_checksum(t, 9);
        }
    }

    /// A table's length is firmware data read before the checksum: anything past the
    /// bound is refused, so it never drives a mapping or a walk.
    #[test]
    fn an_absurd_table_length_is_refused_before_it_is_read() {
        let mut header = [0u8; HEADER_LENGTH];
        header[4..8].copy_from_slice(&MAX_TABLE_LENGTH.to_le_bytes());
        assert_eq!(table_length(&header), Ok(MAX_TABLE_LENGTH));
        header[4..8].copy_from_slice(&(MAX_TABLE_LENGTH + 1).to_le_bytes());
        assert_eq!(
            table_length(&header),
            Err(AcpiError::Length(MAX_TABLE_LENGTH + 1))
        );
        header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(table_length(&header), Err(AcpiError::Length(u32::MAX)));
    }

    /// Every check on an RSDP is reached on mutated input, and whatever passes has the fields
    /// the bytes say (CLAUDE.md §72).
    #[test]
    fn mutated_rsdps_reach_every_check() {
        use crate::testing::{Rng, mutate};
        let good = rsdp(0x1FB7_D0E8);
        let mut rng = Rng::new(0x5244_5350);
        let mut reached = [0u32; 5];
        for case in 0..crate::testing::cases(6000) {
            let mut r = good.clone();
            mutate(&mut r, &mut rng);
            if case % 2 == 0 && r.len() >= 36 {
                fix_checksum(&mut r[..20], 8);
                fix_checksum(&mut r[..36], 32);
            }
            match parse_rsdp(&r) {
                Ok(p) => {
                    assert!(p.revision >= 2 && le32(&r[20..]) >= 36, "case {case}");
                    assert_eq!(p.xsdt.to_le_bytes(), r[24..32], "case {case}");
                    assert!(
                        sums_to_zero(&r[..20]) && sums_to_zero(&r[..36]),
                        "case {case}"
                    );
                    reached[0] += 1;
                }
                Err(AcpiError::RsdpSignature) => reached[1] += 1,
                Err(AcpiError::RsdpChecksum) => reached[2] += 1,
                Err(AcpiError::RsdpRevision(v)) => {
                    assert!(v < 2, "case {case}");
                    reached[3] += 1;
                }
                Err(AcpiError::RsdpLength(l)) => {
                    assert!((l as usize) < RSDP_LENGTH, "case {case}");
                    reached[4] += 1;
                }
                Err(e) => panic!("case {case}: {e:?}"),
            }
        }
        assert!(reached.iter().all(|&n| n > 20), "{reached:?}");
    }

    /// The table-level checks and every MADT list agree with an independent walk of the same
    /// mutated bytes, and an undersized output buffer is an error, never a panic.
    #[test]
    fn mutated_madts_match_an_independent_walk() {
        use crate::testing::{Rng, mutate};
        let base = madt(&[
            &lapic(0, 0, 1),
            &lapic(1, 3, 0),
            &x2apic(300, 1, 7),
            &ioapic(2, 0xFEC0_0000, 0),
            &ioapic(3, 0xFEC0_1000, 24),
            &iso(0, 2, 0),
            &iso(9, 9, 0x000D),
            &[0xD0u8, 4, 0, 0][..],
        ]);
        let mut rng = Rng::new(0x4D41_4454);
        let (mut checked, mut listed, mut overflowed) = (0u32, 0u32, 0u32);
        for case in 0..crate::testing::cases(8000) {
            let mut m = base.clone();
            mutate(&mut m, &mut rng);
            if case % 3 != 0 {
                reseal(&mut m);
            }
            let _ = signature(&m);
            if let Ok(length) = table_length(&m) {
                assert!(length as usize >= HEADER_LENGTH, "case {case}");
            }
            if check_table(&m, b"APIC").is_ok() {
                checked += 1;
                assert!(sums_to_zero(&m) && m[..4] == *b"APIC", "case {case}");
                assert_eq!(le32(&m[4..]) as usize, m.len(), "case {case}");
            }
            let walked = walk(&m, MADT_ENTRIES);
            let of_kind = |kind: u8, min: usize| -> Vec<usize> {
                walked
                    .iter()
                    .flatten()
                    .filter(|&&(_, k, len)| k == kind && len >= min)
                    .map(|&(at, ..)| at)
                    .collect()
            };

            let mut ios = [IoApic::default(); 8];
            match madt_ioapics(&m, &mut ios) {
                Ok(n) => {
                    assert!(walked.is_some(), "case {case}");
                    let expected = of_kind(1, 12);
                    assert_eq!(n, expected.len(), "case {case}");
                    for (io, &at) in ios[..n].iter().zip(&expected) {
                        assert_eq!(io.id, m[at + 2], "case {case}");
                        assert_eq!(io.address, le32(&m[at + 4..]), "case {case}");
                        assert_eq!(io.gsi_base, le32(&m[at + 8..]), "case {case}");
                    }
                    listed += 1;
                }
                Err(AcpiError::MadtEntry(_)) => assert!(walked.is_none(), "case {case}"),
                Err(e) => panic!("case {case}: {e:?}"),
            }

            let mut overrides = [SourceOverride::default(); 8];
            let found = madt_overrides(&m, &mut overrides);
            if let Ok(n) = found {
                let expected = of_kind(2, 10);
                assert_eq!(n, expected.len(), "case {case}");
                for (o, &at) in overrides[..n].iter().zip(&expected) {
                    assert_eq!((o.source_irq, o.gsi), (m[at + 3], le32(&m[at + 4..])));
                }
                // The lookup answers from the list, or with the identity mapping.
                for irq in [0u8, 1, 9, 255] {
                    let (gsi, flags) = gsi_for_irq(irq, &overrides[..n]);
                    match overrides[..n].iter().find(|o| o.source_irq == irq) {
                        Some(o) => assert_eq!((gsi, flags), (o.gsi, o.flags), "case {case}"),
                        None => assert_eq!((gsi, flags), (u32::from(irq), 0), "case {case}"),
                    }
                }
            }

            let mut cpus = [Processor::default(); 8];
            if let Ok(n) = madt_processors(&m, &mut cpus) {
                let mut ids = Vec::new();
                for &(at, kind, len) in walked.iter().flatten() {
                    // A processor structure too short for its fields fails the whole list.
                    let id = match (kind, len) {
                        (0, 8..) => u32::from(m[at + 3]),
                        (9, 16..) => le32(&m[at + 4..]),
                        _ => continue,
                    };
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                assert_eq!(n, ids.len(), "case {case}");
                assert!(cpus[..n].iter().zip(&ids).all(|(p, id)| p.apic_id == *id));
            }

            // Buffers too small for the lists are refused, not overrun.
            let mut tiny = [IoApic::default(); 1];
            match madt_ioapics(&m, &mut tiny) {
                Ok(n) => assert!(n <= 1, "case {case}"),
                Err(AcpiError::TooManyProcessors) => overflowed += 1,
                Err(_) => {}
            }
            let mut none: [SourceOverride; 0] = [];
            let _ = madt_overrides(&m, &mut none);
            let mut no_cpus: [Processor; 0] = [];
            let _ = madt_processors(&m, &mut no_cpus);
        }
        assert!(
            checked > 2000 && listed > 2000 && overflowed > 0,
            "{checked} checked, {listed} listed, {overflowed} overflowed"
        );
    }

    /// The XSDT's list is exactly its whole eight-byte entries, whatever follows the header.
    #[test]
    fn mutated_xsdts_list_exactly_their_whole_entries() {
        use crate::testing::{Rng, mutate};
        let mut body = Vec::new();
        for a in [0x1000u64, 0x2000, u64::MAX] {
            body.extend_from_slice(&a.to_le_bytes());
        }
        let base = table(b"XSDT", &body);
        let mut rng = Rng::new(0x5853_4454);
        for case in 0..crate::testing::cases(4000) {
            let mut x = base.clone();
            mutate(&mut x, &mut rng);
            let got: Vec<u64> = xsdt_entries(&x).collect();
            let expected: Vec<u64> = x
                .get(HEADER_LENGTH..)
                .unwrap_or(&[])
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| u64::from_le_bytes(*c))
                .collect();
            assert_eq!(got, expected, "case {case}");
        }
    }

    /// A FADT is read from its fixed offsets only, and each field follows its own bytes.
    #[test]
    fn mutated_fadts_read_their_fields_from_the_right_bytes() {
        use crate::testing::{Rng, mutate_window};
        let mut body = vec![0u8; 240];
        body[FADT_CENTURY - 36] = 0x32;
        body[FADT_PM_TMR_BLK - 36..FADT_PM_TMR_BLK - 36 + 4]
            .copy_from_slice(&0x608u32.to_le_bytes());
        body[FADT_PM_TMR_LEN - 36] = 4;
        let base = table(b"FACP", &body);
        let mut rng = Rng::new(0x4641_4454);
        let mut with_port = 0u32;
        for case in 0..crate::testing::cases(6000) {
            let mut f = base.clone();
            mutate_window(&mut f, FADT_PM_TMR_BLK..FADT_FLAGS + 4, &mut rng);
            let len = case % (f.len() + 1);
            let cut = if case % 4 == 0 { len } else { f.len() };
            let f = &f[..cut];
            let Ok(fadt) = parse_fadt(f) else {
                assert!(
                    f.len() < FADT_FLAGS + 4,
                    "case {case}: refused a whole table"
                );
                continue;
            };
            assert_eq!(
                fadt.century,
                Some(f[FADT_CENTURY]).filter(|c| (0x0E..=0x7F).contains(c)),
                "case {case}"
            );
            let port = le32(&f[FADT_PM_TMR_BLK..]);
            match fadt.pm_timer_port {
                Some(p) => {
                    assert_eq!(u32::from(p), port, "case {case}");
                    assert!(f[FADT_PM_TMR_LEN] == 4 && p != 0, "case {case}");
                    with_port += 1;
                }
                None => assert!(f[FADT_PM_TMR_LEN] != 4 || port == 0 || port > 0xFFFF),
            }
        }
        assert!(with_port > 500, "{with_port}");
    }

    /// The SRAT lists and the SLIT matrix keep their invariants on mutated tables, and the
    /// lists match an independent walk.
    #[test]
    fn srat_and_slit_mutations_keep_their_invariants() {
        use crate::testing::{Rng, mutate};
        let base_srat = srat(&[
            &cpu0(0, 0, true),
            &cpu0(1, 0x0102_0304, false),
            &cpu2(300, 1, true),
            &mem(0, 0, 0x1000_0000, 1),
            &mem(1, 0x1_0000_0000, 0x1000_0000, 0b110),
            &[6u8, 4, 0, 0][..],
        ]);
        let base_slit = slit(3, &[10, 16, 32, 16, 10, 20, 32, 20, 10]);
        let mut rng = Rng::new(0x5352_4154);
        let (mut cpus_ok, mut mems_ok, mut slits_ok) = (0u32, 0u32, 0u32);
        for case in 0..crate::testing::cases(10_000) {
            let mut s = base_srat.clone();
            mutate(&mut s, &mut rng);
            if case % 3 != 0 {
                reseal(&mut s);
            }
            let walked = walk(&s, SRAT_ENTRIES);
            let mut cpus = [CpuAffinity::default(); 8];
            if let Ok(n) = srat_cpu_affinities(&s, &mut cpus) {
                cpus_ok += 1;
                let expected: Vec<_> = walked
                    .iter()
                    .flatten()
                    .filter(|&&(_, k, len)| (k == 0 && len >= 16) || (k == 2 && len >= 24))
                    .collect();
                assert_eq!(n, expected.len(), "case {case}");
                for (c, &&(at, kind, _)) in cpus[..n].iter().zip(&expected) {
                    let (id, domain, flags) = if kind == 0 {
                        let hi = le32(&[s[at + 9], s[at + 10], s[at + 11], 0]) << 8;
                        (
                            u32::from(s[at + 3]),
                            u32::from(s[at + 2]) | hi,
                            le32(&s[at + 4..]),
                        )
                    } else {
                        (le32(&s[at + 8..]), le32(&s[at + 4..]), le32(&s[at + 12..]))
                    };
                    assert_eq!((c.apic_id, c.proximity_domain), (id, domain), "case {case}");
                    assert_eq!(c.enabled, flags & 1 != 0, "case {case}");
                }
            }
            let mut mems = [MemoryAffinity::default(); 8];
            if let Ok(n) = srat_memory_affinities(&s, &mut mems) {
                mems_ok += 1;
                let expected: Vec<_> = walked
                    .iter()
                    .flatten()
                    .filter(|&&(_, k, _)| k == 1)
                    .collect();
                assert_eq!(n, expected.len(), "case {case}");
                for (m, &&(at, _, len)) in mems[..n].iter().zip(&expected) {
                    assert!(len >= 40, "case {case}");
                    assert_eq!(m.proximity_domain, le32(&s[at + 2..]), "case {case}");
                    assert_eq!(m.base as u32, le32(&s[at + 8..]), "case {case}");
                    assert_eq!(
                        m.length >> 32,
                        u64::from(le32(&s[at + 20..])),
                        "case {case}"
                    );
                }
            }

            let mut t = base_slit.clone();
            mutate(&mut t, &mut rng);
            if case % 3 != 0 {
                reseal(&mut t);
            }
            if let Ok(table) = parse_slit(&t) {
                slits_ok += 1;
                let n = table.localities();
                assert!(44 + n * n <= t.len(), "case {case}: {n} localities");
                assert_eq!(n as u64, u64::from_le_bytes(t[36..44].try_into().unwrap()));
                for i in 0..n.min(8) {
                    for j in 0..n.min(8) {
                        assert_eq!(table.distance(i, j), Some(t[44 + i * n + j]), "case {case}");
                    }
                    assert_eq!(table.distance(i, n), None, "case {case}");
                    assert_eq!(table.distance(n, i), None, "case {case}");
                }
            }
        }
        assert!(
            cpus_ok > 2000 && mems_ok > 2000 && slits_ok > 2000,
            "{cpus_ok} cpu lists, {mems_ok} memory lists, {slits_ok} matrices"
        );
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
