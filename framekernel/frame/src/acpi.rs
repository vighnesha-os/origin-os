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
//!
//! The MADT's, SRAT's and SLIT's readers, and the length and checksum half of
//! [`check_table`], are `origin-acpi-tables`'s (TASKS item 62, step 5; ADR-0019 A6): one parser
//! of those tables in the tree. A structure that breaks a rule of ACPI 6.6 but whose length
//! still locates the next is not used, and the processors, I/O APICs and affinities around it
//! are (the coordinating session's ruling of 2026-11-14: reported, never refused; the census
//! reports it). Only a length that cannot locate the next structure refuses the table.
#![forbid(unsafe_code)]

use origin_acpi_tables::ErrorKind;
use origin_acpi_tables::fadt::{Block, Register};
use origin_acpi_tables::gas::AddressSpace;
use origin_acpi_tables::madt::{Entry, Madt, Polarity, Trigger, override_redirect};
use origin_acpi_tables::srat::{Affinity, Srat};

/// Bytes of an ACPI 2.0+ RSDP that are checksummed and parsed.
pub const RSDP_LENGTH: usize = 36;
/// Bytes of a system description table header.
pub const HEADER_LENGTH: usize = 36;
/// Offset of a table header's revision byte.
pub const HEADER_REVISION: usize = 8;
/// Offset of the first MADT interrupt controller structure.
#[cfg(test)]
const MADT_ENTRIES: usize = 44;
/// The first MADT revision whose Local APIC flags define bit 1, Online Capable: ACPI 6.3 (ECR
/// 1948), MADT revision 5; ACPI 6.6's MADT is revision 7 (§5.2.12, §5.2.12.2). Below it the bit is
/// reserved, and is not read (ADR-0008 Amendment 1, D2; research ledger
/// `docs/research/2026-11-13-any-processor-count.md`, §3a).
pub const MADT_ONLINE_CAPABLE_REVISION: u8 = 5;
/// Offset of the first SRAT resource-affinity structure (36-byte header + 12 reserved).
#[cfg(test)]
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
    /// A field the table cannot be used without breaks its rule, at this offset: a FADT that
    /// names no DSDT (TASKS item 71, ruling 1a).
    Field(usize),
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

/// Checks a whole table: `expected` signature, a length within [`MAX_TABLE_LENGTH`] and equal to
/// `table.len()`, and its checksum. The length and checksum rules are `origin-acpi-tables`'s
/// (`header::check`), so the tree holds one implementation of them.
pub fn check_table(table: &[u8], expected: &[u8; 4]) -> Result<(), AcpiError> {
    let sig = signature(table).ok_or(AcpiError::Length(0))?;
    if &sig != expected {
        return Err(AcpiError::Signature(sig));
    }
    let length = table_length(table)?;
    origin_acpi_tables::header::check(table)
        .map(|_| ())
        .map_err(|e| match e.kind {
            ErrorKind::Checksum => AcpiError::Checksum(sig),
            _ => AcpiError::Length(length),
        })
}

/// What a refusal by `origin-acpi-tables` is, in this module's words: a header's signature, length
/// or checksum as [`check_table`] gives them; anything else at its offset, through `entry` (a
/// walk error, which leaves the next structure unlocated).
fn refused(table: &[u8], e: origin_acpi_tables::Error, entry: fn(usize) -> AcpiError) -> AcpiError {
    let length = || u32::try_from(table.len()).unwrap_or(u32::MAX);
    match e.kind {
        ErrorKind::Signature { found } => AcpiError::Signature(found.0),
        ErrorKind::Length { declared, .. } => {
            AcpiError::Length(u32::try_from(declared).unwrap_or(u32::MAX))
        }
        ErrorKind::TooShort { .. } => AcpiError::Length(length()),
        ErrorKind::Checksum => AcpiError::Checksum(e.table.0),
        _ => entry(e.at),
    }
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

/// What [`find_table`] gives when the XSDT lists no table with the signature asked for.
pub const NOT_LISTED: &str = "table not listed in the XSDT";
/// What [`find_table`] gives when nothing with the signature asked for was found but an entry
/// could not be read, which might have been it.
pub const ENTRY_UNREADABLE: &str = "an XSDT entry could not be read";

/// Reads the table at `addr`, which must have signature `sig`: its header by copy (`copy`), then
/// the whole table lent (`lend`) and checked ([`check_table`]).
pub fn lend_table<'t>(
    addr: u64,
    sig: &[u8; 4],
    copy: &mut impl FnMut(u64, &mut [u8]) -> Result<(), &'static str>,
    lend: &mut impl FnMut(u64, usize) -> Result<&'t [u8], &'static str>,
) -> Result<&'t [u8], &'static str> {
    let mut header = [0u8; HEADER_LENGTH];
    copy(addr, &mut header)?;
    whole_table(addr, sig, &header, lend)
}

/// The whole table at `addr` whose header is `header`, lent and checked.
fn whole_table<'t>(
    addr: u64,
    sig: &[u8; 4],
    header: &[u8],
    lend: &mut impl FnMut(u64, usize) -> Result<&'t [u8], &'static str>,
) -> Result<&'t [u8], &'static str> {
    let len = table_length(header).map_err(|_| "invalid table length")?;
    let all = lend(addr, len as usize)?;
    check_table(all, sig).map_err(|_| "invalid table")?;
    Ok(all)
}

/// A structure the firmware changes after boot, which is copied and never lent (TASKS item 71).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Changing {
    /// The FACS, which firmware in SMM writes (its global lock, §5.2.10.1).
    Facs,
    /// The firmware basic boot performance table, rewritten on resume (§5.2.24.8).
    Fbpt,
    /// The S3 performance table, rewritten on each resume (§5.2.24.10).
    S3pt,
    /// The microcontroller boot performance table the FPDT may point to.
    Mbpt,
}

impl Changing {
    /// Its signature, as the firmware writes it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Facs => "FACS",
            Self::Fbpt => "FBPT",
            Self::S3pt => "S3PT",
            Self::Mbpt => "MBPT",
        }
    }
}

/// The most structures [`changing_ranges`] keeps, which is the most the rule it follows allows:
/// the FACS that §5.2.9 chooses, a second FACS the other pointer names, and the FBPT, S3PT and
/// MBPT the FPDT names, one each (§5.2.24.4 to §5.2.24.6: only one of each pointer record is
/// produced). Only the first valid FADT and FPDT the XSDT lists count, as [`find_table`] takes the
/// first valid table of a signature and the census follows the first FPDT it checks, so no XSDT,
/// however many it lists, fills more (the review of TASKS item 71, T71-1). The lending registry
/// keeps at least as many (`reserved::NEVER_SLOTS`).
pub const CHANGING_MAX: usize = 5;

/// The most bytes a never-lendable range covers: as much as the census copies of such a
/// structure (1 MiB). A structure that declares more is kept at this length.
pub const CHANGING_MAX_LEN: u64 = 1 << 20;

/// The most XSDT entries [`changing_ranges`] reads, each by copy; a longer XSDT is read that far
/// and the rest said not located.
pub const CHANGING_XSDT_ENTRIES: usize = 1024;

/// The longest FADT [`changing_ranges`] copies to check it whole (ACPI 6.6's is 276 bytes); a
/// longer one is taken, unchecked, as the first, and said not located.
const CHANGING_FADT_MAX: usize = 512;

/// The longest FPDT [`changing_ranges`] copies; a longer one is said not located.
const CHANGING_FPDT_MAX: usize = 1024;

/// Where the firmware's structures that change after boot are, as the tables at an RSDP name
/// them, found by [`changing_ranges`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangingRanges {
    ranges: [(Changing, u64, u64); CHANGING_MAX],
    count: usize,
    /// Why not every such structure the tables name was located and recorded, if one was not:
    /// part of the tables could not be read. The kernel then relies on `firmware::bytes`'s
    /// signature check alone for what it did not record, and the census's verdict fails (the
    /// review of TASKS item 71, T71-1). The first reason found is kept.
    pub unlocated: Option<&'static str>,
}

impl ChangingRanges {
    const fn new() -> Self {
        Self {
            ranges: [(Changing::Facs, 0, 0); CHANGING_MAX],
            count: 0,
            unlocated: None,
        }
    }

    /// Each structure found, with its range `[base, end)`.
    pub fn ranges(&self) -> &[(Changing, u64, u64)] {
        self.ranges.get(..self.count).unwrap_or(&[])
    }

    /// Notes why not everything was located, unless a reason is already noted.
    fn note(&mut self, why: &'static str) {
        self.unlocated = self.unlocated.or(Some(why));
    }

    fn push(&mut self, kind: Changing, base: u64, length: u64, min: u64) {
        let length = length.clamp(min, CHANGING_MAX_LEN);
        let Some(end) = base.checked_add(length) else {
            self.note("a structure the firmware changes runs past the address space");
            return;
        };
        if self
            .ranges()
            .iter()
            .any(|&(k, b, e)| k == kind && b == base && e == end)
        {
            return;
        }
        match self.ranges.get_mut(self.count) {
            Some(slot) => {
                *slot = (kind, base, end);
                self.count += 1;
            }
            // Not reached: CHANGING_MAX is the most the rule above allows. Kept to fail closed.
            None => self.note("more structures the firmware changes than expected"),
        }
    }
}

/// The `u32` at offset 4 of `header`: a structure's length field.
fn length_field(header: &[u8]) -> u64 {
    u64::from(u32_at(header, 4).unwrap_or(0))
}

/// The `i`th entry of the XSDT at `xsdt`, by copy: `None` if it could not be read.
fn xsdt_entry(
    xsdt: u64,
    i: usize,
    copy: &mut impl FnMut(u64, &mut [u8]) -> Result<(), &'static str>,
) -> Option<u64> {
    let mut entry = [0u8; 8];
    let at = xsdt.saturating_add(HEADER_LENGTH as u64 + 8 * i as u64);
    copy(at, &mut entry).ok()?;
    Some(u64::from_le_bytes(entry))
}

/// The extent of the table whose header is at `addr`, by copy, if its length is one
/// [`table_length`] takes: what a lend of that table covers.
fn table_extent(
    addr: u64,
    copy: &mut impl FnMut(u64, &mut [u8]) -> Result<(), &'static str>,
) -> Option<(u64, u64, [u8; 4])> {
    let mut header = [0u8; HEADER_LENGTH];
    copy(addr, &mut header).ok()?;
    let length = table_length(&header).ok()?;
    Some((
        addr,
        addr.checked_add(u64::from(length))?,
        signature(&header)?,
    ))
}

/// Locates the firmware's structures that change after boot, as the tables at `rsdp_addr` name
/// them (TASKS item 71), by the rule the kernel's lookups follow (the review of TASKS item 71,
/// T71-1): only the first valid FADT and the first valid FPDT the XSDT lists count. From that
/// FADT, the FACS chosen as §5.2.9 says (`X_FIRMWARE_CTRL` when non-zero, else `FIRMWARE_CTRL`),
/// and a second FACS if the ignored field names one, which §5.2.9 forbids (one whose first bytes
/// are a FACS's signature, so a stale value blocks nothing); from that FPDT, the first FBPT, S3PT
/// and MBPT pointer records. Those ranges are recorded first, the FACS first, each as long as its
/// length field says, kept between its kind's minimum and [`CHANGING_MAX_LEN`].
///
/// Every byte is read by copy (`copy`), into the stack: this takes no lend, so it can neither
/// lend a range before the ones it finds are recorded nor recurse into a table lookup, and it
/// allocates nothing, since the first lookup comes before the kernel's heap. What could not be
/// read to locate them is noted ([`ChangingRanges::unlocated`]): an RSDP or XSDT that could not
/// be read, an XSDT entry or its header that could not be read, an XSDT longer than
/// [`CHANGING_XSDT_ENTRIES`] entries, a first FADT longer than 512 bytes (taken unchecked, its
/// FACS pointers being at offsets 36 and 132), and a first FPDT longer than 1 KiB.
pub fn changing_ranges(
    rsdp_addr: u64,
    copy: &mut impl FnMut(u64, &mut [u8]) -> Result<(), &'static str>,
) -> ChangingRanges {
    use origin_acpi_tables::boot::{Fpdt, FpdtRecord};
    let mut found = ChangingRanges::new();
    let mut rsdp = [0u8; RSDP_LENGTH];
    let Some(xsdt) = copy(rsdp_addr, &mut rsdp)
        .ok()
        .and_then(|()| parse_rsdp(&rsdp).ok())
        .map(|r| r.xsdt)
    else {
        found.note("the RSDP could not be read");
        return found;
    };
    let Some((_, xsdt_end, _)) = table_extent(xsdt, copy) else {
        found.note("the XSDT could not be read");
        return found;
    };
    let entries = usize::try_from(xsdt_end.saturating_sub(xsdt))
        .unwrap_or(0)
        .saturating_sub(HEADER_LENGTH)
        / 8;
    if entries > CHANGING_XSDT_ENTRIES {
        found.note("an XSDT longer than the kernel reads to locate them");
    }
    let entries = entries.min(CHANGING_XSDT_ENTRIES);

    // The candidates, in the order they are recorded: the chosen FACS, the second FACS, then the
    // FPDT's FBPT, S3PT and MBPT.
    let mut candidates: [Option<(Changing, u64)>; CHANGING_MAX] = [None; CHANGING_MAX];
    let (mut fadt_seen, mut fpdt_seen) = (false, false);
    for i in 0..entries {
        if fadt_seen && fpdt_seen {
            break;
        }
        let mut header = [0u8; HEADER_LENGTH];
        let Some(addr) = xsdt_entry(xsdt, i, copy) else {
            found.note("an XSDT entry could not be read to locate them");
            continue;
        };
        if addr == 0 {
            continue;
        }
        if copy(addr, &mut header).is_err() {
            found.note("an XSDT entry could not be read to locate them");
            continue;
        }
        let declared = usize::try_from(length_field(&header)).unwrap_or(usize::MAX);
        let sig = signature(&header);
        // A length `table_length` refuses makes a table `find_table` refuses: not the first valid.
        if matches!(&sig, Some(s) if s == b"FACP" || s == b"FPDT") && table_length(&header).is_err()
        {
            continue;
        }
        match sig {
            Some(sig) if &sig == b"FACP" && !fadt_seen => {
                let mut fadt = [0u8; CHANGING_FADT_MAX];
                let long = declared > fadt.len();
                let len = declared.clamp(HEADER_LENGTH, fadt.len());
                let Some(fadt) = fadt.get_mut(..len) else {
                    continue;
                };
                if copy(addr, fadt).is_err() {
                    found.note("a FADT could not be read to locate the FACS");
                    continue;
                }
                if long {
                    // Too long to check here, so taken as the first: it may be the one the
                    // kernel's lookups take.
                    found.note("a FADT longer than the kernel reads to locate the FACS");
                } else if check_table(fadt, b"FACP").is_err() {
                    // Not valid, so not the one the kernel's lookups take.
                    continue;
                }
                fadt_seen = true;
                let [x, legacy] = origin_acpi_tables::fadt::facs_pointers(fadt);
                let chosen = if x != 0 { x } else { legacy };
                if let Some(slot) = candidates.get_mut(0) {
                    *slot = (chosen != 0).then_some((Changing::Facs, chosen));
                }
                if let Some(slot) = candidates.get_mut(1) {
                    // Both fields named, which §5.2.9 forbids: the ignored one guarded as well,
                    // if what it names is a FACS.
                    *slot = None;
                    let mut head = [0u8; 8];
                    if x != 0
                        && legacy != 0
                        && legacy != x
                        && copy(legacy, &mut head).is_ok()
                        && head[..4] == *b"FACS"
                    {
                        *slot = Some((Changing::Facs, legacy));
                    }
                }
            }
            Some(sig) if &sig == b"FPDT" && !fpdt_seen => {
                let mut fpdt = [0u8; CHANGING_FPDT_MAX];
                let Some(fpdt) = fpdt.get_mut(..declared) else {
                    // Too long to check here, so it may be the first valid one.
                    fpdt_seen = true;
                    found.note("an FPDT longer than the kernel reads to locate its tables");
                    continue;
                };
                if copy(addr, fpdt).is_err() {
                    found.note("an FPDT could not be read to locate its tables");
                    continue;
                }
                let Some(parsed) = check_table(fpdt, b"FPDT")
                    .ok()
                    .and_then(|()| Fpdt::parse(fpdt).ok())
                else {
                    continue;
                };
                fpdt_seen = true;
                for record in parsed.walk() {
                    let (index, kind, target) = match record {
                        Ok((_, FpdtRecord::Fbpt(a))) => (2, Changing::Fbpt, a),
                        Ok((_, FpdtRecord::S3pt(a))) => (3, Changing::S3pt, a),
                        Ok((_, FpdtRecord::Mbpt(a))) => (4, Changing::Mbpt, a),
                        _ => continue,
                    };
                    // Only one of each is produced (§5.2.24.4 to §5.2.24.6): the first counts.
                    if let Some(slot) = candidates.get_mut(index)
                        && slot.is_none()
                    {
                        *slot = (target != 0).then_some((kind, target));
                    }
                }
            }
            _ => {}
        }
    }

    for (kind, base) in candidates.into_iter().flatten() {
        // The least each is: a FACS 64 bytes (§5.2.10), an MBPT's header 24, the others' 8.
        let min = match kind {
            Changing::Facs => 64,
            Changing::Mbpt => 24,
            Changing::Fbpt | Changing::S3pt => 8,
        };
        let mut head = [0u8; 8];
        let declared = copy(base, &mut head).map_or(0, |()| length_field(&head));
        found.push(kind, base, declared, min);
    }
    found
}

/// The table with signature `sig` that the XSDT at `rsdp_addr` lists, as `firmware::acpi_table`
/// gives it: the never-lendable ranges located and recorded first (`located`, [`changing_ranges`],
/// `record`), only if `located` says they have not been, and before anything is lent; then the
/// RSDP and the XSDT lent, and [`find_table`]. `lend` refuses, as `firmware::bytes` does, any
/// range before the ranges are recorded and any that overlaps one (TASKS item 71).
pub fn lookup<'t>(
    rsdp_addr: u64,
    sig: &[u8; 4],
    located: &mut impl FnMut() -> bool,
    record: &mut impl FnMut(&ChangingRanges),
    copy: &mut impl FnMut(u64, &mut [u8]) -> Result<(), &'static str>,
    lend: &mut impl FnMut(u64, usize) -> Result<&'t [u8], &'static str>,
) -> Result<&'t [u8], &'static str> {
    if !located() {
        record(&changing_ranges(rsdp_addr, copy));
    }
    let rsdp = parse_rsdp(lend(rsdp_addr, RSDP_LENGTH)?).map_err(|_| "invalid RSDP")?;
    let xsdt = lend_table(rsdp.xsdt, b"XSDT", copy, lend)?;
    find_table(xsdt, sig, copy, lend)
}

/// The first valid table with signature `sig` that a checked XSDT lists: each entry's header
/// compared by copy (`copy`), and only a table whose signature is `sig` lent (`lend`) and checked.
/// One bad entry does not hide the entries after it; the error is why the last table with this
/// signature was refused, if there was one. An entry whose header could not be copied might have
/// been this table, so a search that finds nothing then says so ([`ENTRY_UNREADABLE`]) rather
/// than [`NOT_LISTED`]. A null entry names nothing.
///
/// No header is lent to be compared (the review's Addendum 2, A2-L1): so an entry naming a
/// structure the firmware changes after boot (the FACS, an FBPT, an S3PT, an MBPT), which may
/// never be lent, is compared like any other and is simply not the table asked for, and an absent
/// table stays [`NOT_LISTED`] on such a machine.
pub fn find_table<'t>(
    xsdt: &[u8],
    sig: &[u8; 4],
    copy: &mut impl FnMut(u64, &mut [u8]) -> Result<(), &'static str>,
    lend: &mut impl FnMut(u64, usize) -> Result<&'t [u8], &'static str>,
) -> Result<&'t [u8], &'static str> {
    let (mut refused, mut unreadable) = (None, false);
    for addr in xsdt_entries(xsdt).filter(|&addr| addr != 0) {
        let mut header = [0u8; HEADER_LENGTH];
        // An entry the kernel cannot read says nothing about the entries after it.
        if copy(addr, &mut header).is_err() {
            unreadable = true;
            continue;
        }
        if signature(&header) == Some(*sig) {
            match whole_table(addr, sig, &header, lend) {
                Ok(all) => return Ok(all),
                Err(why) => refused = Some(why),
            }
        }
    }
    Err(refused.unwrap_or(if unreadable {
        ENTRY_UNREADABLE
    } else {
        NOT_LISTED
    }))
}

/// What the kernel reads from the FADT (signature `"FACP"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fadt {
    /// The CMOS index of the RTC's century register, if the firmware names a usable one
    /// (`CENTURY`, offset 108; 0 means none). Indexes 00h–0Dh are the clock's own
    /// registers and 80h and above are outside the standard bank, so neither is accepted. None
    /// on a hardware-reduced platform, whose OS §5.2.9 has ignore offset 108 (TASKS item 71).
    pub century: Option<u8>,
    /// `IAPC_BOOT_ARCH` (offset 109) bit 5: the platform has no CMOS RTC.
    pub cmos_rtc_absent: bool,
    /// Whether the firmware says the platform has an 8042 keyboard controller
    /// (`IAPC_BOOT_ARCH` bit 1), or `None` if the table predates that flag (FADT revision 1,
    /// ACPI 1.0, where the byte is reserved).
    pub i8042: Option<bool>,
    /// The ACPI power-management timer's I/O port, if the firmware provides one this kernel can
    /// read, as §5.2.9 says to choose it (TASKS item 71): `X_PM_TMR_BLK` when it is non-zero and a
    /// system I/O port; else `PM_TMR_BLK`, the 32-bit field. Either only with `PM_TMR_LEN` 4 and
    /// not on a hardware-reduced platform.
    pub pm_timer_port: Option<u16>,
    /// Whether the PM timer is 32-bit (`FADT.flags` bit 8, `TMR_VAL_EXT`); else 24-bit. False on
    /// a hardware-reduced platform, whose OS ignores the bit.
    pub pm_timer_32bit: bool,
}

/// Offsets in a FADT, for the tests' fixtures.
#[cfg(test)]
const FADT_CENTURY: usize = 108;
#[cfg(test)]
const FADT_IAPC_BOOT_ARCH: usize = 109;
#[cfg(test)]
const FADT_REVISION: usize = 8;
#[cfg(test)]
const FADT_PM_TMR_BLK: usize = 76;
#[cfg(test)]
const FADT_PM_TMR_LEN: usize = 91;
#[cfg(test)]
const FADT_FLAGS: usize = 112;

/// Reads the fields of a checked FADT that the kernel uses, through `origin-acpi-tables`
/// (TASKS item 71: the tree's one FADT parser). Refused, as its `Fadt::parse` refuses: a header
/// that is not a FACP's, a table shorter than ACPI 1.0's 116 bytes ([`AcpiError::Length`]), and
/// a FADT that names no DSDT ([`AcpiError::Field`]), on which no operating system can run and
/// which the census fails (ruling 1a). A field the FADT's other rules make unusable (a PM timer
/// whose length is not 4, say) is simply not given.
pub fn parse_fadt(table: &[u8]) -> Result<Fadt, AcpiError> {
    let f = origin_acpi_tables::fadt::Fadt::parse(table)
        .map_err(|e| refused(table, e, AcpiError::Field))?;
    let reduced = f.flags.hw_reduced_acpi();
    // §5.2.9: the X_ form when it is non-zero and this kernel can use it, which for the PM
    // timer is a system I/O port that keeps its rules; else the 32-bit field (`port_block`, which
    // a faulty X_ form does not stop: the review's T71-4).
    let port = |r: Register| {
        (r.gas.space == AddressSpace::SystemIo)
            .then(|| u16::try_from(r.gas.address).ok())
            .flatten()
    };
    let pm_timer_port = f
        .block(Block::PmTimer)
        .and_then(port)
        .or_else(|| f.port_block(Block::PmTimer).and_then(port));
    Ok(Fadt {
        century: (!reduced && (0x0E..=0x7F).contains(&f.century)).then_some(f.century),
        cmos_rtc_absent: f.iapc_boot_arch.cmos_rtc_not_present(),
        i8042: (f.table().revision() >= 2).then_some(f.iapc_boot_arch.i8042()),
        pm_timer_port,
        pm_timer_32bit: !reduced && f.flags.tmr_val_ext(),
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
    madt_processors_counted(madt, out).map(|(count, _)| count)
}

/// How usable a listing says its processor is: enabled now, then online-capable, then neither.
fn usable(p: &Processor) -> u8 {
    if p.enabled {
        2
    } else {
        u8::from(p.online_capable)
    }
}

/// [`madt_processors`], and how many later listings of an APIC ID already listed disagree with
/// the first on whether it is enabled or online-capable. Of such listings the most usable is kept:
/// an enabled one over the others, an online-capable one over a disabled one (review finding F11
/// of 2026-11-14). Listings that agree are kept once, as before.
pub fn madt_processors_counted(
    madt: &[u8],
    out: &mut [Processor],
) -> Result<(usize, usize), AcpiError> {
    let table = Madt::parse(madt).map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
    let mut disagreeing = 0;
    let reads_online_capable = table.table().revision() >= MADT_ONLINE_CAPABLE_REVISION;
    let mut count = 0;
    for item in table.verdicts() {
        let (_, verdict) = item.map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
        let p = match verdict {
            Ok(Entry::LocalApic {
                uid,
                apic_id,
                flags,
            }) => Processor {
                apic_id: u32::from(apic_id),
                uid: u32::from(uid),
                enabled: flags.enabled,
                online_capable: reads_online_capable && flags.online_capable,
            },
            Ok(Entry::LocalX2Apic {
                x2apic_id,
                flags,
                uid,
            }) => Processor {
                apic_id: x2apic_id,
                uid,
                enabled: flags.enabled,
                online_capable: reads_online_capable && flags.online_capable,
            },
            // Another kind, or a structure that breaks a rule: not a processor this lists.
            _ => continue,
        };
        let known = out
            .get_mut(..count)
            .and_then(|seen| seen.iter_mut().find(|q| q.apic_id == p.apic_id));
        match known {
            Some(q) => {
                if (q.enabled, q.online_capable) != (p.enabled, p.online_capable) {
                    disagreeing += 1;
                    if usable(&p) > usable(q) {
                        *q = p;
                    }
                }
            }
            None => {
                *out.get_mut(count).ok_or(AcpiError::TooManyProcessors)? = p;
                count += 1;
            }
        }
    }
    Ok((count, disagreeing))
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

/// Lists the I/O APICs in a checked MADT into `out`; returns how many. An I/O APIC at an address
/// or interrupt base an earlier one has, or past the 256th, is not listed (`origin-acpi-tables`'
/// rules between the MADT's structures; the census reports it).
pub fn madt_ioapics(madt: &[u8], out: &mut [IoApic]) -> Result<usize, AcpiError> {
    madt_list(madt, out, |entry| match entry {
        Entry::IoApic {
            id,
            address,
            gsi_base,
        } => Some(IoApic {
            id,
            address,
            gsi_base,
        }),
        _ => None,
    })
}

/// Lists every I/O APIC structure the MADT gives that its own rules allow, repeats included: for
/// the platform register pages no PCIe BAR may cover (`pcie::platform_ranges`), which are there
/// whichever of two alike [`madt_ioapics`] keeps for routing (the review's Addendum 2, A2-L2).
/// Returns how many.
pub fn madt_ioapic_pages(madt: &[u8], out: &mut [IoApic]) -> Result<usize, AcpiError> {
    let table = Madt::parse(madt).map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
    let mut count = 0;
    for item in table.alone() {
        let (_, verdict) = item.map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
        if let Ok(Entry::IoApic {
            id,
            address,
            gsi_base,
        }) = verdict
        {
            *out.get_mut(count).ok_or(AcpiError::TooManyProcessors)? = IoApic {
                id,
                address,
                gsi_base,
            };
            count += 1;
        }
    }
    Ok(count)
}

/// Lists the interrupt source overrides into `out`; returns how many. Only an override of an ISA
/// interrupt (bus 0) is listed: §5.2.12.5 gives overrides for ISA alone. **Since step 5 of TASKS
/// item 62 an override for another bus is not listed**, so [`gsi_for_irq`] no longer maps an ISA
/// IRQ through it; before, any override with a matching source was taken, whatever its bus. An
/// override whose only fault is the reserved polarity or trigger encoding (binary 10) keeps its
/// redirect, as the kernel took it before, with flags 0, the bus's defaults, standing for its mode
/// (the review's Addendum 2, A2-L3): dropping it would send the IRQ to its own number (IRQ 0 to
/// interrupt 0, not 2), where only its mode is in doubt. The census reports that fault.
pub fn madt_overrides(madt: &[u8], out: &mut [SourceOverride]) -> Result<usize, AcpiError> {
    let table = Madt::parse(madt).map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
    let mut count = 0;
    for item in table.verdicts() {
        let (raw, verdict) = item.map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
        let listed = match verdict {
            Ok(Entry::SourceOverride { source, gsi, flags }) => Some(SourceOverride {
                source_irq: source,
                gsi,
                flags: inti_bits(flags),
            }),
            Ok(_) => None,
            Err(fault) => override_redirect(&raw, &fault).map(|(source, gsi)| SourceOverride {
                source_irq: source,
                gsi,
                flags: 0,
            }),
        };
        if let Some(o) = listed {
            *out.get_mut(count).ok_or(AcpiError::TooManyProcessors)? = o;
            count += 1;
        }
    }
    Ok(count)
}

/// The MPS INTI flags' bits for a decoded mode (§5.2.12.5): polarity in bits 0 and 1, trigger
/// mode in bits 2 and 3.
fn inti_bits(flags: origin_acpi_tables::madt::Inti) -> u16 {
    let polarity = match flags.polarity {
        Polarity::BusDefault => 0b00,
        Polarity::ActiveHigh => 0b01,
        Polarity::ActiveLow => 0b11,
    };
    let trigger = match flags.trigger {
        Trigger::BusDefault => 0b00,
        Trigger::Edge => 0b01,
        Trigger::Level => 0b11,
    };
    polarity | trigger << 2
}

/// The global system interrupt and flags a legacy ISA `irq` maps to: the matching override,
/// else the identity mapping with default (active-high, edge) flags. `overrides` are ISA's alone,
/// as [`madt_overrides`] lists them.
pub fn gsi_for_irq(irq: u8, overrides: &[SourceOverride]) -> (u32, u16) {
    overrides
        .iter()
        .find(|o| o.source_irq == irq)
        .map_or((u32::from(irq), 0), |o| (o.gsi, o.flags))
}

/// Lists into `out` what `pick` takes from a checked MADT's structures that keep their rules;
/// returns how many. A walk error refuses the table.
fn madt_list<T>(
    madt: &[u8],
    out: &mut [T],
    mut pick: impl FnMut(Entry<'_>) -> Option<T>,
) -> Result<usize, AcpiError> {
    let table = Madt::parse(madt).map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
    let mut count = 0;
    for item in table.verdicts() {
        let (_, verdict) = item.map_err(|e| refused(madt, e, AcpiError::MadtEntry))?;
        if let Some(value) = verdict.ok().and_then(&mut pick) {
            *out.get_mut(count).ok_or(AcpiError::TooManyProcessors)? = value;
            count += 1;
        }
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

/// Lists into `out` what `pick` takes from a checked SRAT's structures that keep their rules;
/// returns how many. A walk error refuses the table.
fn srat_list<T>(
    srat: &[u8],
    out: &mut [T],
    mut pick: impl FnMut(Affinity<'_>) -> Option<T>,
) -> Result<usize, AcpiError> {
    let table = Srat::parse(srat).map_err(|e| refused(srat, e, AcpiError::SratEntry))?;
    let mut count = 0;
    for item in table.verdicts() {
        let (_, verdict) = item.map_err(|e| refused(srat, e, AcpiError::SratEntry))?;
        if let Some(value) = verdict.ok().and_then(&mut pick) {
            *out.get_mut(count).ok_or(AcpiError::TooManyEntries)? = value;
            count += 1;
        }
    }
    Ok(count)
}

/// Lists the processor affinities in a checked SRAT (`check_table(b"SRAT")` already passed)
/// into `out`, from type 0 (local APIC) and type 2 (x2APIC); returns how many. Other structure
/// types, and a structure too short for its type, are not listed.
pub fn srat_cpu_affinities(srat: &[u8], out: &mut [CpuAffinity]) -> Result<usize, AcpiError> {
    srat_list(srat, out, |affinity| match affinity {
        Affinity::LocalApic {
            proximity_domain,
            apic_id,
            enabled,
            ..
        } => Some(CpuAffinity {
            apic_id: u32::from(apic_id),
            proximity_domain,
            enabled,
        }),
        Affinity::X2Apic {
            proximity_domain,
            x2apic_id,
            enabled,
            ..
        } => Some(CpuAffinity {
            apic_id: x2apic_id,
            proximity_domain,
            enabled,
        }),
        _ => None,
    })
}

/// Lists the memory affinities in a checked SRAT (type 1) into `out`; returns how many. An
/// enabled range that wraps past the top of the address space, and a structure too short for its
/// type, are not listed; a disabled range is listed whatever it holds (§5.2.16.2 tells the OS to
/// ignore its contents, and its `enabled` says so).
pub fn srat_memory_affinities(srat: &[u8], out: &mut [MemoryAffinity]) -> Result<usize, AcpiError> {
    srat_list(srat, out, |affinity| match affinity {
        Affinity::Memory {
            proximity_domain,
            base,
            length,
            enabled,
            hot_pluggable,
            non_volatile,
            ..
        } => Some(MemoryAffinity {
            proximity_domain,
            base,
            length,
            enabled,
            hot_pluggable,
            non_volatile,
        }),
        _ => None,
    })
}

/// A checked SLIT (ACPI 6.6 §5.2.17): the locality count and the row-major N×N matrix of
/// relative distances, `origin-acpi-tables`' own.
pub use origin_acpi_tables::platform::Slit;

/// Parses a checked SLIT (`check_table(b"SLIT")` already passed) with `origin-acpi-tables`:
/// refused besides a matrix that does not fit, since step 5 of TASKS item 62, no localities, a
/// diagonal other than 10 and a distance of 0 to 9, which the NUMA distances proof failed on
/// before (the coordinating session's ruling: no change in effect).
pub fn parse_slit(table: &[u8]) -> Result<Slit<'_>, AcpiError> {
    Slit::parse(table).map_err(|e| refused(table, e, |_| AcpiError::Length(0)))
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

    /// Review finding F11 of 2026-11-14: an APIC ID listed again with other flags keeps its most
    /// usable listing (enabled, then online-capable), and each such listing is counted; repeats
    /// that agree are not.
    #[test]
    fn a_processor_listed_again_keeps_its_most_usable_listing() {
        let m = madt(&[
            &lapic(4, 4, 0b10),  // online-capable first
            &x2apic(4, 1, 9),    // then enabled: kept
            &lapic(5, 5, 1),     // enabled first
            &x2apic(5, 0, 8),    // then neither: counted, not kept
            &lapic(6, 6, 0),     // neither
            &x2apic(6, 0b10, 7), // then online-capable: kept
            &lapic(7, 7, 1),
            &x2apic(7, 7, 1), // agrees: not counted
        ]);
        let mut out = [Processor::default(); 8];
        let (n, disagreeing) = madt_processors_counted(&m, &mut out).unwrap();
        assert_eq!((n, disagreeing), (4, 3));
        let kept: Vec<_> = out[..n]
            .iter()
            .map(|p| (p.apic_id, p.enabled, p.online_capable, p.uid))
            .collect();
        assert_eq!(
            kept,
            [
                (4, true, false, 9),
                (5, true, false, 5),
                (6, false, true, 7),
                (7, true, false, 7)
            ]
        );
        assert_eq!(madt_processors(&m, &mut out), Ok(4));
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

    /// An ACPI 6 FADT's 240-byte body after the header, naming a DSDT (offset 40), which
    /// `parse_fadt` needs since TASKS item 71.
    fn fadt_body() -> Vec<u8> {
        let mut body = vec![0u8; 240];
        body[40 - 36..44 - 36].copy_from_slice(&0x7FFD_0000u32.to_le_bytes());
        body
    }

    #[test]
    fn fadt_century_register_and_rtc_presence() {
        // An ACPI 6 FADT is 276 bytes: a 240-byte body after the header.
        let fadt = |century: u8, arch: u16| {
            let mut body = fadt_body();
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
        // ACPI 1.0's FADT is 116 bytes, through the flags dword, so a shorter table is refused.
        let short = |len: usize| {
            let mut t = qemu[..len].to_vec();
            reseal(&mut t);
            t
        };
        assert_eq!(parse_fadt(&short(115)), Err(AcpiError::Length(115)));
        assert!(parse_fadt(&short(116)).is_ok());
    }

    /// TASKS item 71: the FADT read through `origin-acpi-tables`, and what that changes.
    #[test]
    fn the_fadt_is_read_as_the_crate_judges_it() {
        let gas = |space: u8, address: u64| {
            let mut g = vec![space, 32, 0, 3];
            g.extend_from_slice(&address.to_le_bytes());
            g
        };
        let fadt = |edit: &dyn Fn(&mut Vec<u8>)| {
            let mut body = fadt_body();
            body[FADT_CENTURY - 36] = 0x32;
            body[FADT_PM_TMR_BLK - 36..FADT_PM_TMR_BLK - 32]
                .copy_from_slice(&0x0608u32.to_le_bytes());
            body[FADT_PM_TMR_LEN - 36] = 4;
            body[FADT_FLAGS - 36..FADT_FLAGS - 32].copy_from_slice(&(1u32 << 8).to_le_bytes());
            edit(&mut body);
            table(b"FACP", &body)
        };
        let base = parse_fadt(&fadt(&|_| {})).unwrap();
        assert_eq!(
            (base.pm_timer_port, base.century, base.pm_timer_32bit),
            (Some(0x0608), Some(0x32), true)
        );
        // Ruling 1a: a FADT that names no DSDT is refused, so the kernel takes no PM timer,
        // century or 8042 flag from it, where before it did.
        assert_eq!(
            parse_fadt(&fadt(&|b| b[40 - 36..44 - 36].fill(0))),
            Err(AcpiError::Field(40))
        );
        // §5.2.9: the X_ form when non-zero and usable here (a system I/O port)...
        let x_port = parse_fadt(&fadt(&|b| {
            b[208 - 36..220 - 36].copy_from_slice(&gas(1, 0x0408));
        }))
        .unwrap();
        assert_eq!(x_port.pm_timer_port, Some(0x0408), "X_PM_TMR_BLK preferred");
        // ...else the 32-bit field: an X_ form in system memory, which the kernel cannot read.
        let x_memory = parse_fadt(&fadt(&|b| {
            b[208 - 36..220 - 36].copy_from_slice(&gas(0, 0xFED0_0000));
        }))
        .unwrap();
        assert_eq!(
            x_memory.pm_timer_port,
            Some(0x0608),
            "the 32-bit port stands"
        );
        // The review's T71-4: an X_ form with a reserved address space or access size cannot be
        // used either, and the 32-bit port stands, as it did before TASKS item 71.
        for (space, access) in [(0x20, 3), (1, 5)] {
            let faulty = parse_fadt(&fadt(&|b| {
                let mut g = gas(space, 0x0408);
                g[3] = access;
                b[208 - 36..220 - 36].copy_from_slice(&g);
            }))
            .unwrap();
            assert_eq!(faulty.pm_timer_port, Some(0x0608), "{space:#x} {access}");
        }
        // A length other than 4 makes the timer unusable, whichever form names it.
        let bad_length = parse_fadt(&fadt(&|b| b[FADT_PM_TMR_LEN - 36] = 3)).unwrap();
        assert_eq!(bad_length.pm_timer_port, None);
        // A hardware-reduced platform: no PM timer, no century, no TMR_VAL_EXT (§5.2.9 has
        // its OS ignore offsets 46 to 108 and flag bit 8); the RTC and 8042 flags at offset
        // 109 still read.
        let reduced = parse_fadt(&fadt(&|b| {
            b[FADT_FLAGS - 36..FADT_FLAGS - 32]
                .copy_from_slice(&((1u32 << 20) | (1 << 8)).to_le_bytes());
            b[FADT_IAPC_BOOT_ARCH - 36] = 1 << 5;
        }))
        .unwrap();
        assert_eq!(
            (
                reduced.pm_timer_port,
                reduced.century,
                reduced.pm_timer_32bit
            ),
            (None, None, false)
        );
        assert!(reduced.cmos_rtc_absent);
    }

    #[test]
    fn the_fadt_says_whether_there_is_an_8042_from_revision_2() {
        let fadt = |revision: u8, arch: u16| {
            let mut body = fadt_body();
            body[FADT_IAPC_BOOT_ARCH - 36..FADT_IAPC_BOOT_ARCH - 34]
                .copy_from_slice(&arch.to_le_bytes());
            let mut t = table(b"FACP", &body);
            t[FADT_REVISION] = revision;
            fix_checksum(&mut t, 9);
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
        let mut fadt_body = fadt_body();
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
            // The FADT is read through the crate since TASKS item 71, which checks its header:
            // half the cases repaired, so its fields are reached.
            if case % 2 == 0 {
                reseal(&mut f);
            }
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

    /// Since step 5 of TASKS item 62 the MADT is read by `origin-acpi-tables`: a length that cannot
    /// locate the next structure refuses the table; a processor structure too short for its type
    /// is not listed, and the processors around it are.
    #[test]
    fn malformed_madt_structures() {
        for bad in [
            vec![0u8, 0],         // length 0
            vec![0u8, 1],         // length 1
            vec![0u8, 200, 0, 0], // runs past the table
        ] {
            let m = madt(&[&lapic(0, 0, 1), &bad]);
            let mut out = [Processor::default(); 4];
            assert_eq!(
                madt_processors(&m, &mut out),
                Err(AcpiError::MadtEntry(52)),
                "{bad:?}"
            );
        }
        // A lone trailing byte cannot hold a structure header: refused where its length byte would
        // be.
        let mut m = madt(&[&lapic(0, 0, 1)]);
        m.push(0);
        reseal(&mut m);
        let mut out = [Processor::default(); 4];
        assert_eq!(madt_processors(&m, &mut out), Err(AcpiError::MadtEntry(53)));
        // Too short for its type: not listed, and the processors after it are (before step 5,
        // either refused the whole list).
        for short in [
            vec![0u8, 6, 0, 0, 0, 0], // a type-0 structure too short for its flags
            vec![9u8, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], // a type-9 structure too short
        ] {
            let m = madt(&[&lapic(0, 0, 1), &short, &x2apic(300, 1, 7)]);
            let mut out = [Processor::default(); 4];
            assert_eq!(madt_processors(&m, &mut out), Ok(2), "{short:?}");
            assert_eq!((out[0].apic_id, out[1].apic_id), (0, 300));
        }
        // A table that is not a whole MADT is refused as `check_table` would refuse it.
        let mut m = madt(&[&lapic(0, 0, 1)]);
        m[20] ^= 1;
        assert_eq!(
            madt_processors(&m, &mut out),
            Err(AcpiError::Checksum(*b"APIC"))
        );
    }

    /// Step 5's test (the coordinating session's ruling of 2026-11-14): on the kernel's own path,
    /// `madt_processors_counted` as `smp` calls it, a bad NMI, override or wakeup structure before
    /// the processors leaves every processor listed.
    #[test]
    fn a_bad_structure_leaves_the_processor_list_intact() {
        let bad_nmi: &[u8] = &[4, 6, 0xFF, 0, 0, 2]; // LINT2
        let mut bad_override = iso(9, 20, 0x000D);
        bad_override[2] = 1; // bus 1
        let mut bad_wakeup = vec![16u8, 16, 0, 0, 0, 0, 0, 0];
        bad_wakeup.extend_from_slice(&0x7FF0_0008u64.to_le_bytes()); // inside a page
        for bad in [bad_nmi, &bad_override, &bad_wakeup] {
            let m = madt(&[
                bad,
                &lapic(0, 0, 1),
                &x2apic(300, 1, 7),
                &x2apic(301, 1, 8),
                &ioapic(2, 0xFEC0_0000, 0),
            ]);
            assert_eq!(check_table(&m, b"APIC"), Ok(()));
            let mut out = [Processor::default(); 8];
            assert_eq!(madt_processors_counted(&m, &mut out), Ok((3, 0)), "{bad:?}");
            assert_eq!(
                out[..3].iter().map(|p| p.apic_id).collect::<Vec<_>>(),
                [0, 300, 301]
            );
            let mut ios = [IoApic::default(); 4];
            assert_eq!(madt_ioapics(&m, &mut ios), Ok(1), "{bad:?}");
        }
    }

    /// The behaviour change of step 5 (the coordinating session's ruling): an interrupt source
    /// override for a bus other than ISA's is not listed, so `gsi_for_irq` no longer maps an ISA IRQ
    /// through it. Before step 5 an override for bus 1, source 9, sent IRQ 9 to its GSI. One whose
    /// only fault is the reserved polarity or trigger encoding keeps its redirect, its mode the
    /// bus's defaults (the review's Addendum 2, A2-L3): IRQ 0 still reaches interrupt 2.
    #[test]
    fn an_override_for_another_bus_is_not_taken_and_a_reserved_mode_keeps_its_redirect() {
        let mut pci = iso(9, 20, 0x000D);
        pci[2] = 1;
        let reserved_polarity = iso(0, 2, 0b10);
        let reserved_trigger = iso(1, 21, 0b1000);
        let m = madt(&[&lapic(0, 0, 1), &pci, &reserved_polarity, &reserved_trigger]);
        let mut over = [SourceOverride::default(); 4];
        assert_eq!(madt_overrides(&m, &mut over), Ok(2));
        assert_eq!(
            gsi_for_irq(0, &over[..2]),
            (2, 0),
            "IRQ 0 keeps its redirect"
        );
        assert_eq!(gsi_for_irq(1, &over[..2]), (21, 0));
        assert_eq!(
            gsi_for_irq(9, &over[..2]),
            (9, 0),
            "bus 1's override is not ISA's"
        );
        // The flags kept are the defined bits, as the MPS INTI encoding gives them.
        let m = madt(&[&iso(9, 9, 0xFF0D)]);
        assert_eq!(madt_overrides(&m, &mut over), Ok(1));
        assert_eq!(over[0].flags, 0x000D);
    }

    /// The review's Addendum 2, A2-L2: PCIe's platform ranges keep the register page of every I/O
    /// APIC the MADT lists, repeats included, while routing takes the first of two alike.
    #[test]
    fn every_listed_ioapic_page_stays_reserved() {
        let m = madt(&[
            &ioapic(1, 0xFEC0_0000, 0),
            &ioapic(2, 0xFEC0_1000, 0),  // an earlier one's interrupt base
            &ioapic(3, 0xFEC0_0000, 48), // an earlier one's address
            &[1u8, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0][..], // too short to name a page
        ]);
        let mut ios = [IoApic::default(); 8];
        assert_eq!(madt_ioapics(&m, &mut ios), Ok(1));
        assert_eq!(madt_ioapic_pages(&m, &mut ios), Ok(3));
        assert_eq!(
            ios[..3].iter().map(|io| io.address).collect::<Vec<_>>(),
            [0xFEC0_0000, 0xFEC0_1000, 0xFEC0_0000]
        );
    }

    /// The review's Addendum 2, A2-L1: `find_table` compares each XSDT entry's header by copy and
    /// lends only the table asked for, so an XSDT that names the FACS (which `firmware::bytes` never
    /// lends) still lets an absent IVRS read as not listed, which is what keeps PCIe's reserved
    /// ranges (`iommu::acpi_table_if_listed` gives `None` for it, and `register_ranges` goes on).
    #[test]
    fn an_xsdt_naming_the_facs_still_finds_and_misses_tables_as_before() {
        let mut facs = vec![0u8; 64];
        facs[..4].copy_from_slice(b"FACS");
        facs[4..8].copy_from_slice(&64u32.to_le_bytes());
        let dmar = table(b"DMAR", &[0u8; 12]);
        let memory: Vec<(u64, Vec<u8>)> = vec![(0x1000, facs), (0x2000, dmar.clone())];
        let mut entries = Vec::new();
        for addr in [0x1000u64, 0x2000] {
            entries.extend_from_slice(&addr.to_le_bytes());
        }
        let xsdt = table(b"XSDT", &entries);
        let memory: &'static [(u64, Vec<u8>)] = Vec::leak(memory);
        let find = |sig: &[u8; 4], with: &[u8]| {
            let mut lent = Vec::new();
            let mut copy = |addr: u64, out: &mut [u8]| {
                let (_, bytes) = memory
                    .iter()
                    .find(|(a, _)| *a == addr)
                    .ok_or("not mapped")?;
                out.copy_from_slice(bytes.get(..out.len()).ok_or("short")?);
                Ok(())
            };
            let mut lend = |addr: u64, len: usize| -> Result<&'static [u8], &'static str> {
                lent.push(addr);
                let (_, bytes) = memory
                    .iter()
                    .find(|(a, _)| *a == addr)
                    .ok_or("not mapped")?;
                let bytes = bytes.get(..len).ok_or("short")?;
                // As `firmware::bytes`: never the structures the firmware changes after boot.
                if origin_acpi_tables::CHANGES_AFTER_BOOT
                    .iter()
                    .any(|c| bytes.get(..4) == Some(&c[..]))
                {
                    return Err("the firmware changes it after boot");
                }
                Ok(bytes)
            };
            let found = find_table(with, sig, &mut copy, &mut lend);
            (found, lent)
        };
        let (found, lent) = find(b"IVRS", &xsdt);
        assert_eq!(found, Err(NOT_LISTED));
        assert!(lent.is_empty(), "{lent:x?}");
        let (found, lent) = find(b"DMAR", &xsdt);
        assert_eq!(found, Ok(&dmar[..]));
        assert_eq!(lent, [0x2000]);
        // An entry that cannot be read still makes an absent table "could not be read".
        let mut entries = 0x9000u64.to_le_bytes().to_vec();
        entries.extend_from_slice(&0x2000u64.to_le_bytes());
        let (found, _) = find(b"IVRS", &table(b"XSDT", &entries));
        assert_eq!(found, Err(ENTRY_UNREADABLE));
    }

    /// A second I/O APIC at an address or interrupt base an earlier one has is not listed (the
    /// first is), since step 5.
    #[test]
    fn a_repeated_ioapic_is_listed_once() {
        let m = madt(&[
            &ioapic(1, 0xFEC0_0000, 0),
            &ioapic(2, 0xFEC0_0000, 24),
            &ioapic(3, 0xFEC0_1000, 0),
            &ioapic(4, 0xFEC0_2000, 48),
        ]);
        let mut ios = [IoApic::default(); 4];
        assert_eq!(madt_ioapics(&m, &mut ios), Ok(2));
        assert_eq!((ios[0].id, ios[1].id), (1, 4));
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
        // A type-0 structure too short for its fixed 16 bytes is not listed, and those around it
        // are (since step 5 of TASKS item 62; before, it refused the list).
        let s = srat(&[
            &cpu0(0, 0, true),
            &[0u8, 8, 0, 0, 0, 0, 0, 0][..],
            &cpu2(300, 1, true),
        ]);
        let mut cpus = [CpuAffinity::default(); 8];
        assert_eq!(srat_cpu_affinities(&s, &mut cpus), Ok(2));
        assert_eq!((cpus[0].apic_id, cpus[1].apic_id), (0, 300));

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

        // A type-1 memory structure below 40 bytes is not listed; nor is an enabled range that
        // wraps; a disabled one is, whatever it holds (§5.2.16.2).
        let s = srat(&[
            &[1u8, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..],
            &mem(0, u64::MAX - 1, 2, 1),
            &mem(1, u64::MAX - 1, 2, 0),
            &mem(2, 0x1_0000_0000, 0x1000, 1),
        ]);
        let mut mems = [MemoryAffinity::default(); 4];
        assert_eq!(srat_memory_affinities(&s, &mut mems), Ok(2));
        assert_eq!(
            (
                mems[0].proximity_domain,
                mems[0].enabled,
                mems[1].proximity_domain
            ),
            (1, false, 2)
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
    /// mutated bytes, and an undersized output buffer is an error, never a panic. Since step 5 of
    /// TASKS item 62 the walk applies `origin-acpi-tables`' rules on its own: a structure too short
    /// for its type is not listed; an I/O APIC at an address or base an earlier one has is not; an
    /// override is listed only for bus 0, with flags 0 when its mode is the reserved encoding
    /// (A2-L3).
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
            // The readers check the header themselves, as `check_table` does, and need the
            // fixed fields.
            let whole = check_table(&m, b"APIC").is_ok() && m.len() >= MADT_ENTRIES;
            let walked = walk(&m, MADT_ENTRIES).filter(|_| whole);
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
                    let mut kept: Vec<(u32, u32)> = Vec::new();
                    let expected: Vec<usize> = of_kind(1, 12)
                        .into_iter()
                        .filter(|&at| {
                            let (address, base) = (le32(&m[at + 4..]), le32(&m[at + 8..]));
                            let new = kept.iter().all(|&(a, b)| a != address && b != base);
                            if new {
                                kept.push((address, base));
                            }
                            new
                        })
                        .collect();
                    assert_eq!(n, expected.len(), "case {case}");
                    for (io, &at) in ios[..n].iter().zip(&expected) {
                        assert_eq!(io.id, m[at + 2], "case {case}");
                        assert_eq!(io.address, le32(&m[at + 4..]), "case {case}");
                        assert_eq!(io.gsi_base, le32(&m[at + 8..]), "case {case}");
                    }
                    listed += 1;
                }
                Err(AcpiError::MadtEntry(_)) => {
                    assert!(whole && walked.is_none(), "case {case}");
                }
                Err(AcpiError::Signature(_) | AcpiError::Length(_) | AcpiError::Checksum(_)) => {
                    assert!(!whole, "case {case}");
                }
                Err(e) => panic!("case {case}: {e:?}"),
            }

            let mut overrides = [SourceOverride::default(); 8];
            let found = madt_overrides(&m, &mut overrides);
            if let Ok(n) = found {
                let flags_at = |at: usize| u16::from_le_bytes([m[at + 8], m[at + 9]]);
                // Listed: every bus-0 override; one with a reserved mode with flags 0 (A2-L3).
                let expected: Vec<usize> = of_kind(2, 10)
                    .into_iter()
                    .filter(|&at| m[at + 2] == 0)
                    .collect();
                assert_eq!(n, expected.len(), "case {case}");
                for (o, &at) in overrides[..n].iter().zip(&expected) {
                    assert_eq!((o.source_irq, o.gsi), (m[at + 3], le32(&m[at + 4..])));
                    let f = flags_at(at);
                    let defined = f & 0b11 != 0b10 && (f >> 2) & 0b11 != 0b10;
                    assert_eq!(o.flags, if defined { f & 0xF } else { 0 }, "case {case}");
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
                    // A processor structure too short for its fields is not listed.
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
            // A third of the cases are left unsealed, and since step 5 the readers check the header
            // themselves, so those are refused before any list: about 1,800 of 8,000 are listed.
            checked > 2000 && listed > 1500 && overflowed > 0,
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
        let mut body = fadt_body();
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
            let mut f = f[..cut].to_vec();
            // Read through the crate since TASKS item 71, which checks the header: repaired.
            reseal(&mut f);
            let Ok(fadt) = parse_fadt(&f) else {
                assert!(f.len() < 116, "case {case}: refused a whole table");
                continue;
            };
            let reduced = le32(&f[FADT_FLAGS..]) & (1 << 20) != 0;
            assert_eq!(
                fadt.century,
                Some(f[FADT_CENTURY]).filter(|c| !reduced && (0x0E..=0x7F).contains(c)),
                "case {case}"
            );
            let port = le32(&f[FADT_PM_TMR_BLK..]);
            // The fixture's X_PM_TMR_BLK, if the table holds it, is zero, so the 32-bit field
            // names the timer: usable with a length of 4, on a platform that is not
            // hardware-reduced.
            match fadt.pm_timer_port {
                Some(p) => {
                    assert_eq!(u32::from(p), port, "case {case}");
                    assert!(f[FADT_PM_TMR_LEN] == 4 && p != 0 && !reduced, "case {case}");
                    with_port += 1;
                }
                None => assert!(
                    reduced || f[FADT_PM_TMR_LEN] != 4 || port == 0 || port > 0xFFFF,
                    "case {case}"
                ),
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
                // Listed: type 1 at least 40 bytes long, unless enabled with a range that wraps.
                let expected: Vec<_> = walked
                    .iter()
                    .flatten()
                    .filter(|&&(at, k, len)| {
                        let wraps = || {
                            let base = u64::from_le_bytes(s[at + 8..at + 16].try_into().unwrap());
                            let length =
                                u64::from_le_bytes(s[at + 16..at + 24].try_into().unwrap());
                            base.checked_add(length).is_none()
                        };
                        k == 1 && len >= 40 && !(le32(&s[at + 28..]) & 1 != 0 && wraps())
                    })
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
                // Since step 5: at least one locality, 10 on the diagonal, none below 10.
                assert!(n >= 1, "case {case}");
                for i in 0..n.min(8) {
                    assert_eq!(table.distance(i, i), Some(10), "case {case}");
                }
                assert!(t[44..44 + n * n].iter().all(|&d| d >= 10), "case {case}");
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

    /// Firmware memory for the item-2 tests, at [`BASE`]: copies read any part of it, as
    /// `firmware::copy` does.
    const BASE: u64 = 0x7F00_0000;

    fn machine_memory() -> (Vec<u8>, u64) {
        const RSDP_AT: u64 = BASE;
        const XSDT_AT: u64 = BASE + 0x100;
        const FADT_AT: u64 = BASE + 0x1000;
        const FACS_AT: u64 = BASE + 0x2000;
        const FPDT_AT: u64 = BASE + 0x3000;
        const FBPT_AT: u64 = BASE + 0x4000;
        const S3PT_AT: u64 = BASE + 0x5000;
        const MADT_AT: u64 = BASE + 0x6000;
        let mut mem = vec![0u8; 0x8000];
        let mut put = |at: u64, bytes: &[u8]| {
            let at = (at - BASE) as usize;
            mem[at..at + bytes.len()].copy_from_slice(bytes);
        };
        let mut xsdt = Vec::new();
        for e in [FADT_AT, FPDT_AT, MADT_AT] {
            xsdt.extend_from_slice(&e.to_le_bytes());
        }
        put(XSDT_AT, &table(b"XSDT", &xsdt));
        put(RSDP_AT, &rsdp(XSDT_AT));
        // The FADT: its X_FIRMWARE_CTRL names the FACS; its FIRMWARE_CTRL is stale and points
        // into the XSDT, which §5.2.9 says to ignore when the X_ field is non-zero.
        let mut body = fadt_body();
        body[..4].copy_from_slice(&(XSDT_AT as u32).to_le_bytes()); // FIRMWARE_CTRL, offset 36
        body[132 - 36..140 - 36].copy_from_slice(&FACS_AT.to_le_bytes());
        put(FADT_AT, &table(b"FACP", &body));
        let mut facs = vec![0u8; 64];
        facs[..4].copy_from_slice(b"FACS");
        facs[4..8].copy_from_slice(&64u32.to_le_bytes());
        put(FACS_AT, &facs);
        let pointer = |kind: u16, at: u64| {
            let mut r = kind.to_le_bytes().to_vec();
            r.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
            r.extend_from_slice(&at.to_le_bytes());
            r
        };
        put(
            FPDT_AT,
            &table(
                b"FPDT",
                &[pointer(0, FBPT_AT), pointer(1, S3PT_AT)].concat(),
            ),
        );
        let mut fbpt = b"FBPT".to_vec();
        fbpt.extend_from_slice(&0x200u32.to_le_bytes());
        put(FBPT_AT, &fbpt);
        let mut s3pt = b"S3PT".to_vec();
        s3pt.extend_from_slice(&52u32.to_le_bytes());
        put(S3PT_AT, &s3pt);
        put(MADT_AT, &madt(&[&lapic(0, 0, 1)]));
        (mem, RSDP_AT)
    }

    fn copy_from(mem: &[u8]) -> impl FnMut(u64, &mut [u8]) -> Result<(), &'static str> + '_ {
        move |addr: u64, out: &mut [u8]| {
            let at = addr.checked_sub(BASE).ok_or("not firmware memory")? as usize;
            let bytes = mem.get(at..at + out.len()).ok_or("not firmware memory")?;
            out.copy_from_slice(bytes);
            Ok(())
        }
    }

    /// TASKS item 71: the FACS the FADT names as §5.2.9 says (a stale 32-bit pointer ignored), and
    /// the FPDT's FBPT and S3PT, located by copy alone.
    #[test]
    fn the_changing_structures_are_located_by_copy() {
        let (mem, rsdp_at) = machine_memory();
        let found = changing_ranges(rsdp_at, &mut copy_from(&mem));
        assert_eq!(found.unlocated, None);
        assert_eq!(
            found.ranges(),
            &[
                (Changing::Facs, BASE + 0x2000, BASE + 0x2040),
                (Changing::Fbpt, BASE + 0x4000, BASE + 0x4200),
                (Changing::S3pt, BASE + 0x5000, BASE + 0x5034),
            ]
        );
        // An unreadable RSDP locates nothing, and says so.
        let none = changing_ranges(0x10, &mut copy_from(&mem));
        assert_eq!(
            (none.ranges().len(), none.unlocated),
            (0, Some("the RSDP could not be read"))
        );
        // FIRMWARE_CTRL naming a second FACS, which §5.2.9 forbids beside a non-zero
        // X_FIRMWARE_CTRL: both are guarded.
        let mut two = mem.clone();
        two[0x7000..0x7004].copy_from_slice(b"FACS");
        two[0x7004..0x7008].copy_from_slice(&64u32.to_le_bytes());
        two[0x1000 + 36..0x1000 + 40].copy_from_slice(&((BASE + 0x7000) as u32).to_le_bytes());
        reseal(&mut two[0x1000..0x1000 + 276]);
        let found = changing_ranges(rsdp_at, &mut copy_from(&two));
        assert_eq!(
            &found.ranges()[..2],
            &[
                (Changing::Facs, BASE + 0x2000, BASE + 0x2040),
                (Changing::Facs, BASE + 0x7000, BASE + 0x7040),
            ]
        );
        // A FACS that declares far more than it should is kept at CHANGING_MAX_LEN.
        let mut big = mem.clone();
        big[0x2004..0x2008].copy_from_slice(&u32::MAX.to_le_bytes());
        let found = changing_ranges(rsdp_at, &mut copy_from(&big));
        assert_eq!(
            found.ranges()[0],
            (
                Changing::Facs,
                BASE + 0x2000,
                BASE + 0x2000 + CHANGING_MAX_LEN
            )
        );
        // A FADT whose checksum fails is not the one the kernel's lookups take: none is located,
        // and nothing more is said, as the census refuses that FADT itself.
        let mut bad = mem.clone();
        bad[0x1000 + 9] ^= 1;
        let found = changing_ranges(rsdp_at, &mut copy_from(&bad));
        assert!(found.ranges().iter().all(|&(k, ..)| k != Changing::Facs));
        // An XSDT entry that cannot be read is noted (T71-1): it might have been the FADT.
        let mut gap = mem.clone();
        gap[0x100 + 36..0x100 + 44].copy_from_slice(&0x10u64.to_le_bytes());
        reseal(&mut gap[0x100..0x100 + 36 + 24]);
        let found = changing_ranges(rsdp_at, &mut copy_from(&gap));
        assert_eq!(
            found.unlocated,
            Some("an XSDT entry could not be read to locate them")
        );
    }

    /// A machine with the RSDP, an XSDT listing `entries` (each address and its table), and any
    /// other structures, at [`BASE`].
    fn machine_of(entries: &[(u64, Vec<u8>)], others: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut mem = vec![0u8; 0x10000];
        let mut put = |at: u64, bytes: &[u8]| {
            let at = (at - BASE) as usize;
            mem[at..at + bytes.len()].copy_from_slice(bytes);
        };
        let xsdt: Vec<u8> = entries.iter().flat_map(|(a, _)| a.to_le_bytes()).collect();
        put(BASE + 0x100, &table(b"XSDT", &xsdt));
        put(BASE, &rsdp(BASE + 0x100));
        for (at, bytes) in entries.iter().chain(others) {
            put(*at, bytes);
        }
        mem
    }

    /// A FADT whose `X_FIRMWARE_CTRL` is `x` and `FIRMWARE_CTRL` `legacy`.
    fn fadt_naming(x: u64, legacy: u32) -> Vec<u8> {
        let mut body = fadt_body();
        body[..4].copy_from_slice(&legacy.to_le_bytes());
        body[132 - 36..140 - 36].copy_from_slice(&x.to_le_bytes());
        table(b"FACP", &body)
    }

    /// An FPDT of pointer `records` (type, address).
    fn fpdt_of(records: &[(u16, u64)]) -> Vec<u8> {
        let body: Vec<u8> = records
            .iter()
            .flat_map(|&(kind, at)| {
                let mut r = kind.to_le_bytes().to_vec();
                r.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
                r.extend_from_slice(&at.to_le_bytes());
                r
            })
            .collect();
        table(b"FPDT", &body)
    }

    /// A structure the firmware changes: its signature, then its length.
    fn changing_at(sig: &[u8; 4], length: u32) -> Vec<u8> {
        let mut c = sig.to_vec();
        c.extend_from_slice(&length.to_le_bytes());
        c
    }

    /// The review of TASKS item 71, T71-1 (a): only the first valid FADT and FPDT count, as the
    /// kernel's lookups take the first valid table of a signature, and only the first of each
    /// pointer record (the crate refuses a second); so the worst case the rule allows, both FACS
    /// pointers and all three
    /// performance tables, fills [`CHANGING_MAX`] slots and no more, and the registry keeps them
    /// all. The review's probe P1, three FPDTs before the FADT, leaves the FACS guarded.
    #[test]
    fn only_the_first_valid_fadt_and_fpdt_count() {
        let fpdt = |n: u64| BASE + 0x1000 + 0x100 * n;
        let mut bad_fadt = fadt_naming(BASE + 0x9000, 0);
        bad_fadt[9] ^= 1;
        let entries = [
            // Two FBPT pointers, which the crate refuses: not the first valid FPDT.
            (fpdt(0), fpdt_of(&[(0, BASE + 0x4100), (0, BASE + 0x4400)])),
            (
                fpdt(1),
                fpdt_of(&[(0, BASE + 0x4000), (1, BASE + 0x5000), (2, BASE + 0x6000)]),
            ),
            (fpdt(2), fpdt_of(&[(0, BASE + 0x4200), (1, BASE + 0x5100)])),
            (fpdt(3), fpdt_of(&[(0, BASE + 0x4300)])),
            (BASE + 0x2000, bad_fadt),
            (
                BASE + 0x2400,
                fadt_naming(BASE + 0x3000, (BASE + 0x3100) as u32),
            ),
            (BASE + 0x2800, fadt_naming(BASE + 0x3200, 0)),
        ];
        let others = [
            (BASE + 0x3000, changing_at(b"FACS", 64)),
            (BASE + 0x3100, changing_at(b"FACS", 64)),
            (BASE + 0x3200, changing_at(b"FACS", 64)),
            (BASE + 0x4000, changing_at(b"FBPT", 0x40)),
            (BASE + 0x4100, changing_at(b"FBPT", 0x40)),
            (BASE + 0x4400, changing_at(b"FBPT", 0x40)),
            (BASE + 0x4200, changing_at(b"FBPT", 0x40)),
            (BASE + 0x4300, changing_at(b"FBPT", 0x40)),
            (BASE + 0x5000, changing_at(b"S3PT", 0x40)),
            (BASE + 0x5100, changing_at(b"S3PT", 0x40)),
            (BASE + 0x6000, changing_at(b"MBPT", 0x40)),
            (BASE + 0x9000, changing_at(b"FACS", 64)),
        ];
        let mem = machine_of(&entries, &others);
        let found = changing_ranges(BASE, &mut copy_from(&mem));
        assert_eq!(found.unlocated, None);
        assert_eq!(
            found.ranges(),
            &[
                (Changing::Facs, BASE + 0x3000, BASE + 0x3040),
                (Changing::Facs, BASE + 0x3100, BASE + 0x3140),
                (Changing::Fbpt, BASE + 0x4000, BASE + 0x4040),
                (Changing::S3pt, BASE + 0x5000, BASE + 0x5040),
                (Changing::Mbpt, BASE + 0x6000, BASE + 0x6040),
            ]
        );
        assert_eq!(found.ranges().len(), CHANGING_MAX);
        let mut registry = crate::reserved::Lending::new();
        let ranges: Vec<(u64, u64)> = found.ranges().iter().map(|&(_, b, e)| (b, e)).collect();
        let kept = registry.never_lend(&ranges);
        assert_eq!((kept.recorded, kept.untracked), (CHANGING_MAX, 0));
        // P1: a lend into the FACS's middle is refused.
        assert!(!registry.lend(BASE + 0x3010, BASE + 0x3018));
        // Nothing of the later FADT's or FPDTs', nor the invalid FADT's or FPDT's, is taken: the
        // census refuses an invalid one itself.
        for later in [0x3200, 0x4100, 0x4200, 0x4300, 0x4400, 0x5100, 0x9000] {
            assert!(
                !registry.never_lent(BASE + later, BASE + later + 8),
                "{later:#x}"
            );
        }
        // The other order, the FADTs first, and a later FPDT that names a kind the first does
        // not: still only the first of each counts, so no MBPT and only the first FADT's FACS.
        let entries = [
            (BASE + 0x2400, fadt_naming(BASE + 0x3000, 0)),
            (BASE + 0x2800, fadt_naming(BASE + 0x3200, 0)),
            (fpdt(0), fpdt_of(&[(0, BASE + 0x4000), (1, BASE + 0x5000)])),
            (fpdt(1), fpdt_of(&[(0, BASE + 0x4200), (2, BASE + 0x6000)])),
        ];
        let mem = machine_of(&entries, &others);
        let found = changing_ranges(BASE, &mut copy_from(&mem));
        assert_eq!(found.unlocated, None);
        assert_eq!(
            found.ranges(),
            &[
                (Changing::Facs, BASE + 0x3000, BASE + 0x3040),
                (Changing::Fbpt, BASE + 0x4000, BASE + 0x4040),
                (Changing::S3pt, BASE + 0x5000, BASE + 0x5040),
            ]
        );
    }

    /// TASKS item 71, ruling addition 2: the first lookup locates and records the changing
    /// structures before it lends anything, a later one does not locate them again, and nothing
    /// is lent over them, the FACS's middle included.
    #[test]
    fn the_first_lookup_records_before_it_lends() {
        use crate::reserved::Lending;
        use std::cell::RefCell;
        let (mem, rsdp_at) = machine_memory();
        let mem: &'static [u8] = Vec::leak(mem);
        let registry = RefCell::new(Lending::new());
        let events = RefCell::new(Vec::<&'static str>::new());
        let mut located = || registry.borrow().located();
        let mut record = |found: &ChangingRanges| {
            events.borrow_mut().push("record");
            let ranges: Vec<(u64, u64)> = found.ranges().iter().map(|&(_, b, e)| (b, e)).collect();
            registry.borrow_mut().never_lend(&ranges);
        };
        let mut copy = |addr: u64, out: &mut [u8]| {
            events.borrow_mut().push("copy");
            copy_from(mem)(addr, out)
        };
        let mut lend = |addr: u64, len: usize| -> Result<&'static [u8], &'static str> {
            events.borrow_mut().push("lend");
            let at = (addr - BASE) as usize;
            let bytes = mem.get(at..at + len).ok_or("not firmware memory")?;
            if registry.borrow_mut().lend(addr, addr + len as u64) {
                Ok(bytes)
            } else {
                Err("refused")
            }
        };
        // Before any lookup, nothing may be lent.
        assert_eq!(lend(BASE + 0x6000, 36), Err("refused"));
        events.borrow_mut().clear();
        let madt_found = lookup(
            rsdp_at,
            b"APIC",
            &mut located,
            &mut record,
            &mut copy,
            &mut lend,
        );
        assert_eq!(madt_found.map(|t| &t[..4]), Ok(&b"APIC"[..]));
        let log = events.borrow().clone();
        let recorded = log.iter().position(|&e| e == "record").unwrap();
        let first_lend = log.iter().position(|&e| e == "lend").unwrap();
        assert!(recorded < first_lend, "{log:?}");
        assert!(log[..recorded].iter().all(|&e| e == "copy"), "{log:?}");
        // A second lookup finds the ranges recorded and does not locate them again.
        events.borrow_mut().clear();
        assert!(
            lookup(
                rsdp_at,
                b"FPDT",
                &mut located,
                &mut record,
                &mut copy,
                &mut lend
            )
            .is_ok()
        );
        assert!(!events.borrow().contains(&"record"));
        // Nothing is lent over a changing structure: its start, its middle, or across its end.
        for (at, len) in [
            (0x2000, 36),
            (0x2010, 16),
            (0x203F, 8),
            (0x4100, 36),
            (0x5000, 8),
        ] {
            assert_eq!(lend(BASE + at, len), Err("refused"), "{at:#x}");
        }
        // The stale FIRMWARE_CTRL, which points into the XSDT, blocked nothing.
        assert!(lend(BASE + 0x100, 36).is_ok());
    }
}
