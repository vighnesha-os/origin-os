//! The census of the firmware's ACPI tables at boot (ADR-0019 A6, area A; TASKS item 62; the
//! "ACPI tables" proof, ADR-0013).
//!
//! Every table the XSDT lists, the RSDT, the XSDT itself, the FADT's DSDT and FACS, and the tables
//! the FPDT points to are read through `firmware::bytes` (read-only, and only memory the memory
//! map calls the firmware's) and checked whole by `origin-acpi-tables`. One line each says what
//! the table is and what was found; a last line counts them and gives the verdict. Nothing the
//! kernel does depends on these results yet: the tables it already uses are read where they always
//! were (`acpi.rs`).

use core::fmt::{self, Write};

use origin_acpi_tables::boot::{Fpdt, FpdtRecord, PerformanceKind, PerformanceTable};
use origin_acpi_tables::census::{self, Class};
use origin_acpi_tables::facs::Facs;
use origin_acpi_tables::fadt::Fadt;
use origin_acpi_tables::header::{HEADER_LEN, Header};
use origin_acpi_tables::{Error, Signature};

use crate::console::Console;

/// Where every line of the census starts.
const LINE: &str = "origin-kernel: acpi: ";

/// What the census found, table by table.
#[derive(Default)]
struct Counts {
    tables: u32,
    checked: u32,
    refused: u32,
    other: u32,
    unreadable: u32,
}

/// Text from firmware bytes: printable ASCII as itself, anything else as `?`.
struct Ascii<'a>(&'a [u8]);

impl fmt::Display for Ascii<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0 {
            let c = if b.is_ascii_graphic() || b == b' ' {
                char::from(b)
            } else {
                '?'
            };
            f.write_char(c)?;
        }
        Ok(())
    }
}

/// A refusal, as a census line gives it.
struct Refused(Error);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refused at {:#x}: {}", self.0.at, self.0.kind)
    }
}

/// Reads the table whose header is at `addr`: the header first, then as many bytes as it says, up
/// to the most the kernel maps. `Err` if the memory cannot be read.
fn read_table(addr: u64) -> Result<(&'static [u8], Header), &'static str> {
    let first = crate::firmware::bytes(addr, HEADER_LEN)?;
    let header = Header::read(first).map_err(|_| "a short header")?;
    if header.length > crate::acpi::MAX_TABLE_LENGTH {
        return Err("longer than the kernel maps a table");
    }
    let length = usize::try_from(header.length)
        .unwrap_or(HEADER_LEN)
        .max(HEADER_LEN);
    Ok((crate::firmware::bytes(addr, length)?, header))
}

/// Prints one table's line and counts it.
fn report(
    com1: &mut Console,
    counts: &mut Counts,
    header: &Header,
    result: Result<Class, Error>,
) -> bool {
    counts.tables = counts.tables.saturating_add(1);
    let _ = write!(
        com1,
        "{LINE}{} rev {}, {} bytes, OEM \"{}\": ",
        header.signature,
        header.revision,
        header.length,
        Ascii(&header.oem_id)
    );
    match result {
        Ok(class) => {
            if matches!(class, Class::Checked(_)) {
                counts.checked = counts.checked.saturating_add(1);
            } else {
                counts.other = counts.other.saturating_add(1);
            }
            let _ = writeln!(com1, "{class}");
            true
        }
        Err(e) => {
            counts.refused = counts.refused.saturating_add(1);
            let _ = writeln!(com1, "{}", Refused(e));
            false
        }
    }
}

/// Checks the table at `addr` with the census and reports it; `expected`, if given, is the
/// signature the pointer to it names. Returns the table and whether it was checked.
fn census_at(
    com1: &mut Console,
    counts: &mut Counts,
    what: &str,
    addr: u64,
    expected: Option<&[u8; 4]>,
) -> Option<&'static [u8]> {
    let (bytes, header) = match read_table(addr) {
        Ok(read) => read,
        Err(why) => {
            counts.unreadable = counts.unreadable.saturating_add(1);
            let _ = writeln!(com1, "{LINE}{what}: not readable: {why}");
            return None;
        }
    };
    let result = match expected {
        Some(sig) if !header.signature.is(sig) => Err(Error::new(
            Signature::new(sig),
            0,
            origin_acpi_tables::ErrorKind::Signature {
                found: header.signature,
            },
        )),
        _ => census::check(bytes).map(|c| c.class),
    };
    report(com1, counts, &header, result).then_some(bytes)
}

/// The census. Passes when no table is refused, every table the XSDT lists could be read, and the
/// FADT, the DSDT and (unless the platform is hardware-reduced) the FACS are there and checked.
pub(crate) fn prove(com1: &mut Console, rsdp_addr: u64) -> bool {
    let started = crate::time::monotonic_nanos();
    let mut counts = Counts::default();
    let Ok(rsdp_bytes) = crate::firmware::bytes(rsdp_addr, crate::acpi::RSDP_LENGTH) else {
        let _ = writeln!(
            com1,
            "{LINE}the RSDP cannot be read; consistent false: acpi tables failed"
        );
        return false;
    };
    let Ok(rsdp) = crate::acpi::parse_rsdp(rsdp_bytes) else {
        let _ = writeln!(
            com1,
            "{LINE}the RSDP is invalid; consistent false: acpi tables failed"
        );
        return false;
    };
    // The RSDT, for ACPI 1.0's operating systems: checked and listed, never followed (§5.2.8).
    let rsdt = rsdp_bytes
        .get(16..20)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes);
    let rsdt = if rsdt == 0 {
        None
    } else {
        census_at(com1, &mut counts, "RSDT", u64::from(rsdt), Some(b"RSDT"))
    };
    let Some(xsdt) = census_at(com1, &mut counts, "XSDT", rsdp.xsdt, Some(b"XSDT")) else {
        let _ = writeln!(
            com1,
            "{LINE}no checked XSDT; consistent false: acpi tables failed"
        );
        return false;
    };
    let (mut fadt, mut fpdt, mut unreadable_entries) = (None, None, 0u32);
    for addr in crate::acpi::xsdt_entries(xsdt).filter(|&a| a != 0) {
        let before = counts.unreadable;
        if let Some(bytes) = census_at(com1, &mut counts, "an XSDT entry", addr, None) {
            match bytes.get(..4) {
                Some(b"FACP") if fadt.is_none() => fadt = Some(bytes),
                Some(b"FPDT") if fpdt.is_none() => fpdt = Some(bytes),
                _ => {}
            }
        }
        if counts.unreadable != before {
            unreadable_entries = unreadable_entries.saturating_add(1);
        }
    }
    let fadt = fadt.and_then(|b| Fadt::parse(b).ok());
    let (mut dsdt_ok, mut facs_ok) = (false, false);
    let mut reduced = false;
    if let Some(fadt) = &fadt {
        reduced = fadt.flags.hw_reduced_acpi();
        let _ = writeln!(
            com1,
            "{LINE}FADT: profile {:?}, SCI {}, hardware-reduced {}, reset register {}, PM timer {}",
            fadt.preferred_pm_profile,
            fadt.sci_int,
            reduced,
            fadt.reset().map_or("none", |_| "given"),
            fadt.block(origin_acpi_tables::fadt::Block::PmTimer)
                .map_or("none", |_| "given"),
        );
        if let Some(dsdt) = fadt.dsdt_address() {
            dsdt_ok = census_at(com1, &mut counts, "the DSDT", dsdt, Some(b"DSDT")).is_some();
        }
        if let Some(facs) = fadt.facs_address() {
            facs_ok = check_facs(com1, &mut counts, facs);
        }
        // §5.2.7 and §5.2.8: the root tables' OEM table ID is the FADT's. Reported, not judged.
        let id = fadt.table().header().oem_table_id;
        let agree = |root: Option<&[u8]>| {
            root.and_then(|b| Header::read(b).ok())
                .is_none_or(|h| h.oem_table_id == id)
        };
        let _ = writeln!(
            com1,
            "{LINE}the RSDT's and XSDT's OEM table IDs are the FADT's: {}",
            agree(rsdt) && agree(Some(xsdt))
        );
    }
    if let Some(fpdt) = fpdt {
        performance_tables(com1, &mut counts, fpdt);
    }
    let consistent = counts.refused == 0
        && unreadable_entries == 0
        && fadt.is_some()
        && dsdt_ok
        && (facs_ok || reduced);
    let micros = crate::time::monotonic_nanos().saturating_sub(started) / 1000;
    let _ = writeln!(
        com1,
        "{LINE}{} tables: {} checked, {} refused, {} another specification's or area's, {} not readable; FADT {}, DSDT {}, FACS {}; {} us; consistent {}{}",
        counts.tables,
        counts.checked,
        counts.refused,
        counts.other,
        counts.unreadable,
        fadt.is_some(),
        dsdt_ok,
        if reduced && !facs_ok {
            "not needed"
        } else if facs_ok {
            "true"
        } else {
            "false"
        },
        micros,
        consistent,
        if consistent {
            ""
        } else {
            ": acpi tables failed"
        },
    );
    consistent
}

/// Reads and checks the FACS, which has no header: its first eight bytes give its length.
fn check_facs(com1: &mut Console, counts: &mut Counts, addr: u64) -> bool {
    let read = crate::firmware::bytes(addr, 8).and_then(|head| {
        let length = head
            .get(4..8)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, u32::from_le_bytes);
        if length > crate::acpi::MAX_TABLE_LENGTH {
            return Err("longer than the kernel maps a table");
        }
        crate::firmware::bytes(addr, usize::try_from(length).unwrap_or(8).max(8))
    });
    counts.tables = counts.tables.saturating_add(1);
    match read.map(Facs::parse) {
        Err(why) => {
            counts.unreadable = counts.unreadable.saturating_add(1);
            let _ = writeln!(com1, "{LINE}the FACS: not readable: {why}");
            false
        }
        Ok(Ok(facs)) => {
            counts.checked = counts.checked.saturating_add(1);
            let _ = writeln!(
                com1,
                "{LINE}FACS version {}, {} bytes: checked: 64-bit wake {}, global lock owned {}",
                facs.version,
                facs.length,
                facs.flags.wake_64bit_supported(),
                facs.global_lock.owned
            );
            true
        }
        Ok(Err(e)) => {
            counts.refused = counts.refused.saturating_add(1);
            let _ = writeln!(com1, "{LINE}the FACS: {}", Refused(e));
            false
        }
    }
}

/// The tables the FPDT points to: each read only if the memory map calls its memory the
/// firmware's, and otherwise reported as not readable, which fails nothing.
fn performance_tables(com1: &mut Console, counts: &mut Counts, fpdt: &'static [u8]) {
    let Ok(fpdt) = Fpdt::parse(fpdt) else { return };
    for record in fpdt.walk() {
        let (kind, addr) = match record {
            Ok((_, FpdtRecord::Fbpt(a))) => (PerformanceKind::Fbpt, a),
            Ok((_, FpdtRecord::S3pt(a))) => (PerformanceKind::S3pt, a),
            Ok((_, FpdtRecord::Mbpt(a))) => (PerformanceKind::Mbpt, a),
            _ => continue,
        };
        let name = Ascii(kind.signature());
        let read = crate::firmware::bytes(addr, kind.header_len()).and_then(|head| {
            let length = head
                .get(4..8)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map_or(0, u32::from_le_bytes);
            if length > crate::acpi::MAX_TABLE_LENGTH {
                return Err("longer than the kernel maps a table");
            }
            crate::firmware::bytes(
                addr,
                usize::try_from(length).unwrap_or(0).max(kind.header_len()),
            )
        });
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(why) => {
                let _ = writeln!(com1, "{LINE}{name}: not readable: {why}");
                continue;
            }
        };
        counts.tables = counts.tables.saturating_add(1);
        match PerformanceTable::parse(bytes, kind) {
            Ok(t) => {
                counts.checked = counts.checked.saturating_add(1);
                let records = t.records().count();
                let _ = writeln!(
                    com1,
                    "{LINE}{name}, {} bytes: checked: {records} records",
                    bytes.len()
                );
            }
            Err(e) => {
                counts.refused = counts.refused.saturating_add(1);
                let _ = writeln!(com1, "{LINE}{name}: {}", Refused(e));
            }
        }
    }
}
