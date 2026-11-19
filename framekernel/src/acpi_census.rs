//! The census of the firmware's ACPI tables at boot (ADR-0019 A6, area A; TASKS item 62; the
//! "ACPI tables" proof, ADR-0013).
//!
//! Every table the XSDT lists, the RSDT, the XSDT itself and the FADT's DSDT are read through
//! `firmware::bytes` (read-only, and only memory the memory map calls the firmware's), which
//! lends them for the kernel's life: the firmware does not change them after boot. The FACS and
//! the tables the FPDT points to, which it does change (the global lock; records rewritten on
//! resume), are copied out with volatile reads through `firmware::copy` and never lent (the
//! independent review's F2). Each is checked whole by `origin-acpi-tables`. One line each says
//! what the table is and what was found; a last line counts them and gives the verdict. Nothing
//! the kernel does depends on these results yet: the tables it already uses are read where they
//! always were (`acpi.rs`).
//!
//! The census reads through a [`Firmware`]: at boot the kernel's, on the host tables built in
//! memory, so that every branch of the verdict ([`Findings::consistent`]) is tested there (the
//! review's F7).

use alloc::vec::Vec;
use core::fmt::Write;

use origin_acpi_tables::boot::{Fpdt, FpdtRecord, PerformanceKind, PerformanceTable};
use origin_acpi_tables::census::{self, Class};
use origin_acpi_tables::facs::Facs;
use origin_acpi_tables::fadt::{Block, Fadt, Field};
use origin_acpi_tables::header::{HEADER_LEN, Header};
use origin_acpi_tables::madt::Madt;
use origin_acpi_tables::srat::Srat;
use origin_acpi_tables::{CHANGES_AFTER_BOOT, Error, Signature};

use crate::acpi::ChangingRanges;
use crate::acpi_text::{Ascii, Refused, Sig, Whose};
use origin_frame::reserved::Never;

/// Where every line of the census starts.
const LINE: &str = "origin-kernel: acpi: ";

/// The most bytes the census copies of a firmware structure that changes after boot: the FACS
/// (64 bytes in every version ACPI 6.6 gives) and the FPDT's tables, whose length is the space the
/// firmware set aside, which may be far more than their records (§5.2.24.8; the review's addendum,
/// N2). A larger one is "too large to judge", never "not readable".
const COPY_MAX: usize = 1 << 20;

/// The longest header a copied structure has: an MBPT's 24 bytes.
const COPY_HEADER_MAX: usize = 24;

/// Why a structure was not copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uncopied {
    /// Its memory could not be read, or no memory could be had for the copy (N6).
    Unreadable(&'static str),
    /// It declares more than [`COPY_MAX`] bytes (N2).
    TooLarge(usize),
}

/// Where the census reads the firmware's memory.
pub trait Firmware {
    /// Lends `len` bytes at physical `addr` for the kernel's life: only for the tables the
    /// firmware does not change after boot (`firmware::bytes`).
    fn lend(&mut self, addr: u64, len: usize) -> Result<&'static [u8], &'static str>;
    /// Copies `out.len()` bytes at physical `addr`, lending nothing (`firmware::copy`).
    fn copy(&mut self, addr: u64, out: &mut [u8]) -> Result<(), &'static str>;
    /// Whether [`Firmware::lend`] refuses `len` bytes at `addr` for its guard over the firmware's
    /// structures that change after boot, asked without lending (`firmware::guards`; the review
    /// of TASKS item 71, T71-7). A reader that cannot ask (the AML proof's, which runs no
    /// census) says it would not refuse, so a census run through it fails rather than passes.
    fn guards(&mut self, _addr: u64, _len: usize) -> bool {
        false
    }
    /// A monotonic clock in nanoseconds, for the time the census took.
    fn now(&self) -> u64;
    /// What the kernel's first table lookup located as the firmware's structures that change
    /// after boot, and what the lending registry did with them (`firmware::changing`; TASKS item
    /// 71), if it has run.
    fn changing(&self) -> Option<(ChangingRanges, Never)> {
        None
    }
}

/// The kernel's firmware memory.
#[cfg(target_os = "none")]
struct Boot;

#[cfg(target_os = "none")]
impl Firmware for Boot {
    fn lend(&mut self, addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
        crate::firmware::bytes(addr, len)
    }
    fn copy(&mut self, addr: u64, out: &mut [u8]) -> Result<(), &'static str> {
        crate::firmware::copy(addr, out)
    }
    fn guards(&mut self, addr: u64, len: usize) -> bool {
        crate::firmware::guards(addr, len)
    }
    fn now(&self) -> u64 {
        crate::time::monotonic_nanos()
    }
    fn changing(&self) -> Option<(ChangingRanges, Never)> {
        crate::firmware::changing().copied()
    }
}

/// The census at boot, over the kernel's firmware memory.
#[cfg(target_os = "none")]
pub(crate) fn prove(com1: &mut crate::console::Console, rsdp_addr: u64) -> bool {
    census(com1, &mut Boot, rsdp_addr)
}

/// What the census found, and its verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Findings {
    /// Tables seen.
    pub tables: u32,
    /// Tables checked whole.
    pub checked: u32,
    /// Tables refused.
    pub refused: u32,
    /// Structures or fields that broke a rule, reported and not used, in checked tables.
    pub reported: u32,
    /// Of those, the ones in structures and fields the kernel relies on ([`relied_on`]): each
    /// fails the verdict, though its table is not refused (the review's addendum, N1).
    pub relied: u32,
    /// Tables named and only their headers checked: another specification's or area's, for other
    /// processors, an OEM's, unknown, or in a layout ACPI 6.6 no longer gives.
    pub other: u32,
    /// Tables that could not be read.
    pub unreadable: u32,
    /// Structures copied rather than lent that declare more than the census copies (N2).
    pub too_large: u32,
    /// Lends over a structure the firmware changes after boot that the guard would not refuse,
    /// asked without lending, at every boot (TASKS item 71; the review's T71-7). Any fails the
    /// verdict.
    pub lendable_changing: u32,
    /// XSDT entries that were zero, which name no table (the review's F8).
    pub null_entries: u32,
    /// XSDT entries that could not be read: each might have been any table.
    pub unreadable_entries: u32,
    /// A FADT was found and checked.
    pub fadt: bool,
    /// The FADT's DSDT was found and checked.
    pub dsdt: bool,
    /// The FADT's FACS was found and checked.
    pub facs: bool,
    /// The FADT says the platform is hardware-reduced, so it needs no FACS.
    pub reduced: bool,
}

impl Findings {
    /// The verdict: no table refused, every table the XSDT lists read, no fault in a structure or
    /// field the kernel relies on (N1), a FADT, a DSDT, a FACS unless the platform is
    /// hardware-reduced, and no lend over a structure the firmware changes that the guard would
    /// make (TASKS item 71). Not counted against it: a fault reported in a structure or field
    /// nothing
    /// in the kernel uses, so that ordinary firmware bugs in unused tables do not mark a machine
    /// degraded (the coordinating session's ruling on N1); a null XSDT entry, which names no
    /// table; and a table the FPDT points to that cannot be read or is too large to judge, which
    /// nothing needs.
    #[must_use]
    pub const fn consistent(&self) -> bool {
        self.refused == 0
            && self.unreadable_entries == 0
            && self.relied == 0
            && self.lendable_changing == 0
            && self.fadt
            && self.dsdt
            && (self.facs || self.reduced)
    }

    /// Counts one table's result.
    fn count(&mut self, result: &Result<Class, Error>) {
        self.tables = self.tables.saturating_add(1);
        match result {
            Ok(Class::Checked(s)) => {
                self.checked = self.checked.saturating_add(1);
                self.reported = self.reported.saturating_add(s.reported());
            }
            Ok(_) => self.other = self.other.saturating_add(1),
            Err(_) => self.refused = self.refused.saturating_add(1),
        }
    }
}

/// How many of a checked table's reported faults are in structures the kernel relies on (the
/// review's addendum, N1, and the coordinating session's ruling): the MADT's processors, I/O APICs
/// and interrupt source overrides (types 0, 1, 2 and 9), which `smp` and `irq` read; the SRAT's
/// processor and memory affinities (types 0, 1 and 2), which the NUMA map reads. The FADT's are
/// counted where the census reads it ([`relied_on_fadt`]).
pub fn relied_on(table: &[u8]) -> u32 {
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    match table.get(..4) {
        Some(b"APIC") => Madt::parse(table).map_or(0, |m| {
            count(
                m.verdicts()
                    .filter(|v| matches!(v, Ok((raw, Err(_))) if matches!(raw.kind, 0 | 1 | 2 | 9)))
                    .count(),
            )
        }),
        Some(b"SRAT") => Srat::parse(table).map_or(0, |s| {
            count(
                s.verdicts()
                    .filter(|v| matches!(v, Ok((raw, Err(_))) if matches!(raw.kind, 0..=2)))
                    .count(),
            )
        }),
        _ => 0,
    }
}

/// The firmware's structures that change after boot, as the kernel's first table lookup located
/// them and recorded them as never lent (TASKS item 71): each with its range, inclusive, and what
/// the registry could not keep or found lent before. Reported; it decides nothing.
struct NeverLent<'c>(&'c ChangingRanges, Never);

impl core::fmt::Display for NeverLent<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("never lent, as the firmware changes them after boot:")?;
        if self.0.ranges().is_empty() {
            f.write_str(" none named")?;
        }
        for (i, &(kind, base, end)) in self.0.ranges().iter().enumerate() {
            let sep = if i == 0 { " " } else { ", " };
            write!(
                f,
                "{sep}{} {base:#x}-{:#x}",
                kind.name(),
                end.saturating_sub(1)
            )?;
        }
        write!(f, "; {} kept", self.1.recorded)?;
        if self.1.untracked != 0 {
            write!(f, ", {} not kept for want of room", self.1.untracked)?;
        }
        if let Some((base, end)) = self.1.already_lent {
            write!(
                f,
                ", one overlapping {base:#x}-{:#x}, lent before",
                end.saturating_sub(1)
            )?;
        }
        if let Some(why) = self.0.unlocated {
            write!(f, "; not all located: {why}")?;
        }
        Ok(())
    }
}

/// The PM timer as the census line words it (TASKS item 71): `X_PM_TMR_BLK` and the 32-bit
/// `PM_TMR_BLK` as the FADT gives them, and the port the kernel's own reader
/// (`acpi::parse_fadt`) takes, as §5.2.9 says to choose it.
struct PmTimer<'f, 'a>(&'f Fadt<'a>, Option<&'a [u8]>);

impl core::fmt::Display for PmTimer<'_, '_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let fadt = self.0;
        match fadt.x_block(Block::PmTimer) {
            None => f.write_str("X_PM_TMR_BLK absent")?,
            Some(g) if g.is_null() => f.write_str("X_PM_TMR_BLK zero")?,
            Some(g) => write!(f, "X_PM_TMR_BLK {:?} {:#x}", g.space, g.address)?,
        }
        match fadt.port_block(Block::PmTimer) {
            Some(r) => write!(f, ", PM_TMR_BLK port {:#x}", r.gas.address)?,
            None => f.write_str(", PM_TMR_BLK none")?,
        }
        let taken = self
            .1
            .and_then(|b| crate::acpi::parse_fadt(b).ok())
            .and_then(|k| k.pm_timer_port);
        match taken {
            Some(port) => write!(f, "; the kernel's: port {port:#x}"),
            None => f.write_str("; the kernel's: none"),
        }
    }
}

/// The FADT's fields the kernel relies on that break a rule (N1): its FACS pointer, its PM1a
/// event and control blocks and PM timer, and its reset register. The census reports every
/// faulty field on the FADT's line; these also fail the verdict.
pub fn relied_on_fadt(fadt: &Fadt<'_>) -> u32 {
    let fields = [
        Field::Facs,
        Field::Block(Block::Pm1aEvent),
        Field::Block(Block::Pm1aControl),
        Field::Block(Block::PmTimer),
        Field::Reset,
    ];
    let faulty = fields.iter().filter(|&&f| !fadt.usable(f)).count();
    u32::try_from(faulty).unwrap_or(u32::MAX)
}

/// A table as the census read it: lent, or copied because the firmware changes it after boot.
enum Read {
    /// Lent for the kernel's life (`firmware::bytes`): a table the firmware does not change.
    Lent(&'static [u8]),
    /// Copied, never lent: a structure [`CHANGES_AFTER_BOOT`] names, wherever it was named
    /// from (the review's addendum, N3).
    Copied(Vec<u8>),
}

/// Reads the table whose header is at `addr`: the header first, by copy, then, if its signature
/// is not one of [`CHANGES_AFTER_BOOT`], as many bytes as it says, lent, up to the most the kernel
/// maps; if it is, the whole structure copied and never lent (N3). `Err` if the memory cannot be
/// read.
fn read_table(fw: &mut impl Firmware, addr: u64) -> Result<(Read, Header), Uncopied> {
    let mut first = [0u8; HEADER_LEN];
    fw.copy(addr, &mut first).map_err(Uncopied::Unreadable)?;
    let header = Header::read(&first).map_err(|_| Uncopied::Unreadable("a short header"))?;
    if CHANGES_AFTER_BOOT.iter().any(|s| header.signature.is(s)) {
        return Ok((Read::Copied(copy_structure(fw, addr, 8)?), header));
    }
    if header.length > crate::acpi::MAX_TABLE_LENGTH {
        return Err(Uncopied::Unreadable("longer than the kernel maps a table"));
    }
    let length = usize::try_from(header.length)
        .unwrap_or(HEADER_LEN)
        .max(HEADER_LEN);
    let lent = fw.lend(addr, length).map_err(Uncopied::Unreadable)?;
    Ok((Read::Lent(lent), header))
}

/// Checks a copied structure as what its signature says it is: the FACS, or one of the FPDT's
/// tables.
fn check_changing(copy: &[u8], signature: Signature) -> Result<(), Error> {
    let kind = match &signature.0 {
        b"FACS" => return Facs::parse(copy).map(|_| ()),
        b"FBPT" => PerformanceKind::Fbpt,
        b"S3PT" => PerformanceKind::S3pt,
        _ => PerformanceKind::Mbpt,
    };
    PerformanceTable::parse(copy, kind).map(|_| ())
}

/// Copies the structure at `addr`, whose length is the `u32` at offset 4 of its first `header`
/// bytes, with volatile reads and without lending it (the review's F2). The copy is read again
/// whole, so what is checked is one consistent snapshot of the length it declares. Its buffer is
/// allocated fallibly: no memory is "not readable: no memory", never the kernel's allocation-error
/// path (N6).
fn copy_structure(fw: &mut impl Firmware, addr: u64, header: usize) -> Result<Vec<u8>, Uncopied> {
    let mut head = [0u8; COPY_HEADER_MAX];
    let head = head
        .get_mut(..header.clamp(8, COPY_HEADER_MAX))
        .ok_or(Uncopied::Unreadable(
            "a header longer than the census copies",
        ))?;
    fw.copy(addr, head).map_err(Uncopied::Unreadable)?;
    let length = head
        .get(4..8)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes);
    let length = usize::try_from(length).unwrap_or(usize::MAX).max(header);
    if length > COPY_MAX {
        return Err(Uncopied::TooLarge(length));
    }
    let mut all = Vec::new();
    all.try_reserve_exact(length)
        .map_err(|_| Uncopied::Unreadable("no memory"))?;
    all.resize(length, 0);
    fw.copy(addr, &mut all).map_err(Uncopied::Unreadable)?;
    Ok(all)
}

/// Prints one table's line and counts it. Returns whether it was checked.
fn report(
    out: &mut impl Write,
    found: &mut Findings,
    header: &Header,
    result: Result<Class, Error>,
) -> bool {
    found.count(&result);
    let _ = write!(
        out,
        "{LINE}{} rev {}, {} bytes, OEM \"{}\": ",
        Sig(header.signature),
        header.revision,
        header.length,
        Ascii(&header.oem_id)
    );
    match result {
        Ok(class) => {
            let _ = writeln!(out, "{}", Whose(class));
            true
        }
        Err(e) => {
            let _ = writeln!(out, "{}", Refused(e));
            false
        }
    }
}

/// Checks the table at `addr` with the census and reports it; `expected`, if given, is the
/// signature the pointer to it names. Returns the table if it was checked.
fn census_at(
    out: &mut impl Write,
    fw: &mut impl Firmware,
    found: &mut Findings,
    what: &str,
    addr: u64,
    expected: Option<&[u8; 4]>,
) -> Option<&'static [u8]> {
    let (read, header) = match read_table(fw, addr) {
        Ok(read) => read,
        Err(Uncopied::Unreadable(why)) => {
            found.tables = found.tables.saturating_add(1);
            found.unreadable = found.unreadable.saturating_add(1);
            let _ = writeln!(out, "{LINE}{what}: not readable: {why}");
            return None;
        }
        Err(Uncopied::TooLarge(length)) => {
            found.tables = found.tables.saturating_add(1);
            found.too_large = found.too_large.saturating_add(1);
            let _ = writeln!(out, "{LINE}{what}: {length} bytes, too large to judge");
            return None;
        }
    };
    let misnamed = expected.filter(|sig| !header.signature.is(sig)).map(|sig| {
        Error::new(
            Signature::new(sig),
            0,
            origin_acpi_tables::ErrorKind::Signature {
                found: header.signature,
            },
        )
    });
    let bytes = match read {
        Read::Lent(bytes) => bytes,
        Read::Copied(copy) => {
            // A structure the firmware changes after boot, named here as a table (N3): copied,
            // never lent, and judged as what it is.
            found.tables = found.tables.saturating_add(1);
            let result = misnamed.map_or_else(|| check_changing(&copy, header.signature), Err);
            let _ = write!(
                out,
                "{LINE}{what}: {}, {} bytes, copied, never lent: ",
                Sig(header.signature),
                copy.len()
            );
            match result {
                Ok(()) => {
                    found.checked = found.checked.saturating_add(1);
                    let _ = writeln!(out, "checked");
                }
                Err(e) => {
                    found.refused = found.refused.saturating_add(1);
                    let _ = writeln!(out, "{}", Refused(e));
                }
            }
            return None;
        }
    };
    let result = match misnamed {
        Some(e) => Err(e),
        None => census::check(bytes).map(|c| c.class),
    };
    report(out, found, &header, result).then_some(bytes)
}

/// The census over `fw`, from the RSDP at `rsdp_addr`. Passes as [`Findings::consistent`] says.
pub fn census(out: &mut impl Write, fw: &mut impl Firmware, rsdp_addr: u64) -> bool {
    let started = fw.now();
    let mut found = Findings::default();
    let Ok(rsdp_bytes) = fw.lend(rsdp_addr, crate::acpi::RSDP_LENGTH) else {
        let _ = writeln!(
            out,
            "{LINE}the RSDP cannot be read; consistent false: acpi tables failed"
        );
        return false;
    };
    let Ok(rsdp) = crate::acpi::parse_rsdp(rsdp_bytes) else {
        let _ = writeln!(
            out,
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
        census_at(out, fw, &mut found, "RSDT", u64::from(rsdt), Some(b"RSDT"))
    };
    let Some(xsdt) = census_at(out, fw, &mut found, "XSDT", rsdp.xsdt, Some(b"XSDT")) else {
        let _ = writeln!(
            out,
            "{LINE}no checked XSDT; consistent false: acpi tables failed"
        );
        return false;
    };
    let (mut fadt, mut fpdt) = (None, None);
    for (i, addr) in crate::acpi::xsdt_entries(xsdt).enumerate() {
        if addr == 0 {
            found.null_entries = found.null_entries.saturating_add(1);
            let _ = writeln!(out, "{LINE}XSDT entry {i}: null, names no table");
            continue;
        }
        let before = found.unreadable;
        if let Some(bytes) = census_at(out, fw, &mut found, "an XSDT entry", addr, None) {
            found.relied = found.relied.saturating_add(relied_on(bytes));
            match bytes.get(..4) {
                Some(b"FACP") if fadt.is_none() => fadt = Some(bytes),
                Some(b"FPDT") if fpdt.is_none() => fpdt = Some(bytes),
                _ => {}
            }
        }
        if found.unreadable != before {
            found.unreadable_entries = found.unreadable_entries.saturating_add(1);
        }
    }
    let fadt_bytes = fadt;
    let fadt = fadt.and_then(|b| Fadt::parse(b).ok());
    found.fadt = fadt.is_some();
    if let Some(fadt) = &fadt {
        found.reduced = fadt.flags.hw_reduced_acpi();
        found.relied = found.relied.saturating_add(relied_on_fadt(fadt));
        let _ = writeln!(
            out,
            "{LINE}FADT: profile {:?}, SCI {}, hardware-reduced {}, reset register {}, PM timer {}",
            fadt.preferred_pm_profile,
            fadt.sci_int,
            found.reduced,
            fadt.reset().map_or("none", |_| "given"),
            PmTimer(fadt, fadt_bytes),
        );
        if let Some(dsdt) = fadt.dsdt_address() {
            found.dsdt = census_at(out, fw, &mut found, "the DSDT", dsdt, Some(b"DSDT")).is_some();
        }
        if let Some(facs) = fadt.facs_address() {
            found.facs = check_facs(out, fw, &mut found, facs);
        }
        // §5.2.7 and §5.2.8: the root tables' OEM table ID is the FADT's. Reported, not judged.
        let id = fadt.table().header().oem_table_id;
        let agree = |root: Option<&[u8]>| {
            root.and_then(|b| Header::read(b).ok())
                .is_none_or(|h| h.oem_table_id == id)
        };
        let _ = writeln!(
            out,
            "{LINE}the RSDT's and XSDT's OEM table IDs are the FADT's: {}",
            agree(rsdt) && agree(Some(xsdt))
        );
    }
    if let Some(fpdt) = fpdt {
        performance_tables(out, fw, &mut found, fpdt);
    }
    if let Some((changing, kept)) = fw.changing() {
        // The guard, at every boot, asked without lending (T71-7): a lend at each structure's
        // start, in its middle, and from before its start into it must be refused.
        let mut not_refused = None;
        for &(kind, base, end) in changing.ranges() {
            let middle = base.saturating_add(end.saturating_sub(base) / 2);
            let before = base.checked_sub(8).map(|b| (b, 16));
            for (at, len) in [Some((base, HEADER_LEN)), Some((middle, 8)), before]
                .into_iter()
                .flatten()
            {
                if !fw.guards(at, len) {
                    found.lendable_changing = found.lendable_changing.saturating_add(1);
                    not_refused.get_or_insert((kind, at, at.saturating_add(len as u64)));
                }
            }
        }
        let _ = write!(
            out,
            "{LINE}{}; a lend over each refused: {}",
            NeverLent(&changing, kept),
            not_refused.is_none()
        );
        if let Some((kind, at, end)) = not_refused {
            let _ = write!(
                out,
                " (not over the {} at {at:#x}-{:#x})",
                kind.name(),
                end.saturating_sub(1)
            );
        }
        let _ = writeln!(out);
    }
    let consistent = found.consistent();
    let micros = fw.now().saturating_sub(started) / 1000;
    let _ = writeln!(
        out,
        "{LINE}{} tables: {} checked, {} refused, {} reported, {} of them where the kernel relies on it, {} with only their headers checked, {} not readable, {} too large to judge, {} null XSDT entries, {} lends over what the firmware changes; FADT {}, DSDT {}, FACS {}; {} us; consistent {}{}",
        found.tables,
        found.checked,
        found.refused,
        found.reported,
        found.relied,
        found.other,
        found.unreadable,
        found.too_large,
        found.null_entries,
        found.lendable_changing,
        found.fadt,
        found.dsdt,
        if found.reduced && !found.facs {
            "not needed"
        } else if found.facs {
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

/// Copies and checks the FACS, which has no header: its first eight bytes give its length.
fn check_facs(
    out: &mut impl Write,
    fw: &mut impl Firmware,
    found: &mut Findings,
    addr: u64,
) -> bool {
    let read = copy_structure(fw, addr, 8);
    found.tables = found.tables.saturating_add(1);
    match read.as_deref().map(Facs::parse) {
        Err(Uncopied::Unreadable(why)) => {
            found.unreadable = found.unreadable.saturating_add(1);
            let _ = writeln!(out, "{LINE}the FACS: not readable: {why}");
            false
        }
        Err(Uncopied::TooLarge(length)) => {
            found.too_large = found.too_large.saturating_add(1);
            let _ = writeln!(out, "{LINE}the FACS: {length} bytes, too large to judge");
            false
        }
        Ok(Ok(facs)) => {
            found.checked = found.checked.saturating_add(1);
            let _ = writeln!(
                out,
                "{LINE}FACS version {}, {} bytes: checked: 64-bit wake {}, global lock owned {}",
                facs.version,
                facs.length,
                facs.flags.wake_64bit_supported(),
                facs.global_lock.owned
            );
            true
        }
        Ok(Err(e)) => {
            found.refused = found.refused.saturating_add(1);
            let _ = writeln!(out, "{LINE}the FACS: {}", Refused(e));
            false
        }
    }
}

/// The tables the FPDT points to: each copied only if the memory map calls its memory the
/// firmware's, and otherwise reported as not readable, which fails nothing.
fn performance_tables(
    out: &mut impl Write,
    fw: &mut impl Firmware,
    found: &mut Findings,
    fpdt: &'static [u8],
) {
    let Ok(fpdt) = Fpdt::parse(fpdt) else { return };
    for record in fpdt.walk() {
        let (kind, addr) = match record {
            Ok((_, FpdtRecord::Fbpt(a))) => (PerformanceKind::Fbpt, a),
            Ok((_, FpdtRecord::S3pt(a))) => (PerformanceKind::S3pt, a),
            Ok((_, FpdtRecord::Mbpt(a))) => (PerformanceKind::Mbpt, a),
            _ => continue,
        };
        let name = Ascii(kind.signature());
        let bytes = match copy_structure(fw, addr, kind.header_len()) {
            Ok(bytes) => bytes,
            Err(Uncopied::Unreadable(why)) => {
                let _ = writeln!(out, "{LINE}{name}: not readable: {why}");
                continue;
            }
            Err(Uncopied::TooLarge(length)) => {
                found.tables = found.tables.saturating_add(1);
                found.too_large = found.too_large.saturating_add(1);
                let _ = writeln!(out, "{LINE}{name}: {length} bytes, too large to judge");
                continue;
            }
        };
        found.tables = found.tables.saturating_add(1);
        match PerformanceTable::parse(&bytes, kind) {
            Ok(t) => {
                found.checked = found.checked.saturating_add(1);
                let records = t.records().count();
                let _ = writeln!(
                    out,
                    "{LINE}{name}, {} bytes: checked: {records} records",
                    bytes.len()
                );
            }
            Err(e) => {
                found.refused = found.refused.saturating_add(1);
                let _ = writeln!(out, "{LINE}{name}: {}", Refused(e));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    /// Where the tables of a [`Machine`] start in physical memory.
    const BASE: u64 = 0x7F00_0000;
    const RSDP: u64 = BASE;
    const XSDT: u64 = BASE + 0x100;
    const FADT: u64 = BASE + 0x1000;
    const DSDT: u64 = BASE + 0x2000;
    const FACS: u64 = BASE + 0x3000;
    const FPDT: u64 = BASE + 0x4000;
    const FBPT: u64 = BASE + 0x5000;
    const OEM: u64 = BASE + 0x6000;
    /// An address no firmware memory covers.
    const NOWHERE: u64 = 0x1_0000_0000;

    /// A table with `signature`, `revision` and `body`, its length and checksum set.
    fn table(signature: &[u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[..4].copy_from_slice(signature);
        t[8] = revision;
        t[10..16].copy_from_slice(b"ORIGIN");
        t[16..24].copy_from_slice(b"CENSUS  ");
        t.extend_from_slice(body);
        seal(&mut t);
        t
    }

    fn seal(t: &mut [u8]) {
        let len = u32::try_from(t.len()).unwrap();
        t[4..8].copy_from_slice(&len.to_le_bytes());
        t[9] = 0;
        let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
    }

    /// The firmware's tables as a machine with fixed hardware gives them: an RSDP, an XSDT
    /// listing a FADT and whatever else a test adds, the FADT's DSDT and FACS.
    struct Machine {
        memory: Vec<u8>,
        entries: Vec<u64>,
        fadt: Vec<u8>,
        /// The lending registry's guard broken: what the census's live check must catch.
        guard_broken: bool,
    }

    impl Machine {
        fn new() -> Self {
            let mut m = Self {
                memory: vec![0u8; 0x8000 + (256 << 10)],
                entries: vec![FADT],
                fadt: Vec::new(),
                guard_broken: false,
            };
            // A FADT of 276 bytes, with fixed hardware, its FACS and DSDT given.
            let mut f = vec![0u8; 240];
            let mut put = |at: usize, bytes: &[u8]| {
                f[at - 36..at - 36 + bytes.len()].copy_from_slice(bytes);
            };
            put(36, &u32::try_from(FACS).unwrap().to_le_bytes());
            put(40, &u32::try_from(DSDT).unwrap().to_le_bytes());
            put(46, &9u16.to_le_bytes());
            put(56, &0x600u32.to_le_bytes());
            put(64, &0x604u32.to_le_bytes());
            put(76, &0x608u32.to_le_bytes());
            put(80, &0x620u32.to_le_bytes());
            put(88, &[4, 2, 0, 4, 16]);
            put(112, &1u32.to_le_bytes());
            m.fadt = table(b"FACP", 6, &f);
            m.put(DSDT, &table(b"DSDT", 2, &[0x10, 0x05]));
            let mut facs = vec![0u8; 64];
            facs[..4].copy_from_slice(b"FACS");
            facs[4..8].copy_from_slice(&64u32.to_le_bytes());
            facs[32] = 2;
            m.put(FACS, &facs);
            m
        }

        fn put(&mut self, addr: u64, bytes: &[u8]) {
            let at = usize::try_from(addr - BASE).unwrap();
            self.memory[at..at + bytes.len()].copy_from_slice(bytes);
        }

        /// Edits the FADT, then seals it again.
        fn fadt(&mut self, edit: impl FnOnce(&mut Vec<u8>)) {
            edit(&mut self.fadt);
            seal(&mut self.fadt);
        }

        /// Runs the census: its verdict, its lines, and the addresses it lent.
        fn run(mut self) -> (bool, String, Vec<u64>) {
            let fadt = core::mem::take(&mut self.fadt);
            self.put(FADT, &fadt);
            let mut xsdt = Vec::new();
            for e in &self.entries {
                xsdt.extend_from_slice(&e.to_le_bytes());
            }
            let xsdt = table(b"XSDT", 1, &xsdt);
            self.put(XSDT, &xsdt);
            let mut rsdp = vec![0u8; 36];
            rsdp[..8].copy_from_slice(b"RSD PTR ");
            rsdp[9..15].copy_from_slice(b"ORIGIN");
            rsdp[15] = 2;
            rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
            rsdp[24..32].copy_from_slice(&XSDT.to_le_bytes());
            let sum = |b: &[u8]| 0u8.wrapping_sub(b.iter().fold(0u8, |s, &x| s.wrapping_add(x)));
            rsdp[8] = sum(&rsdp[..20]);
            rsdp[32] = sum(&rsdp);
            self.put(RSDP, &rsdp);
            let bytes: &'static [u8] = Vec::leak(self.memory);
            // As the kernel's first lookup does: the changing structures located by copy and
            // recorded with a lending registry (TASKS item 71).
            let mut copy = |addr: u64, out: &mut [u8]| {
                let at = usize::try_from(addr.checked_sub(BASE).ok_or("no")?).map_err(|_| "no")?;
                out.copy_from_slice(bytes.get(at..at + out.len()).ok_or("no")?);
                Ok(())
            };
            let found = crate::acpi::changing_ranges(RSDP, &mut copy);
            let ranges: Vec<(u64, u64)> = found.ranges().iter().map(|&(_, b, e)| (b, e)).collect();
            let mut registry = origin_frame::reserved::Lending::new();
            let kept = registry.never_lend(&ranges);
            let mut fw = Memory {
                bytes,
                lent: Vec::new(),
                changing: Some((found, kept)),
                registry,
                guard_broken: self.guard_broken,
            };
            let mut out = String::new();
            let verdict = census(&mut out, &mut fw, RSDP);
            (verdict, out, fw.lent)
        }
    }

    /// Firmware memory at [`BASE`]: lent and copied from one buffer, and every lend made recorded.
    struct Memory {
        bytes: &'static [u8],
        lent: Vec<u64>,
        changing: Option<(ChangingRanges, Never)>,
        /// The lending registry the kernel's lends go through, with the changing ranges in it.
        registry: origin_frame::reserved::Lending,
        /// A lend the registry refused is still made: the guard broken, for a test.
        guard_broken: bool,
    }

    impl Memory {
        fn range(&self, addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
            let at = addr
                .checked_sub(BASE)
                .and_then(|a| usize::try_from(a).ok())
                .ok_or("not the firmware's memory")?;
            self.bytes
                .get(at..at.checked_add(len).ok_or("wraps")?)
                .ok_or("not the firmware's memory")
        }
    }

    impl Firmware for Memory {
        fn lend(&mut self, addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
            let bytes = self.range(addr, len)?;
            // As `firmware::bytes`: through the lending registry (TASKS item 71).
            if !self.registry.lend(addr, addr + len as u64) && !self.guard_broken {
                return Err("refused by the lending registry");
            }
            // As `firmware::bytes` refuses them (N3).
            if bytes
                .get(..4)
                .is_some_and(|s| CHANGES_AFTER_BOOT.iter().any(|c| s == *c))
            {
                return Err("the firmware changes it after boot: copy it, never lend it");
            }
            self.lent.push(addr);
            Ok(bytes)
        }
        fn copy(&mut self, addr: u64, out: &mut [u8]) -> Result<(), &'static str> {
            out.copy_from_slice(self.range(addr, out.len())?);
            Ok(())
        }
        fn guards(&mut self, addr: u64, len: usize) -> bool {
            // As `firmware::guards`: the registry's check, and the signature (N3).
            !self.guard_broken
                && (self.registry.guards(addr, addr + len as u64)
                    || self
                        .range(addr, 4)
                        .is_ok_and(|s| CHANGES_AFTER_BOOT.iter().any(|c| s == *c)))
        }
        fn now(&self) -> u64 {
            0
        }
        fn changing(&self) -> Option<(ChangingRanges, Never)> {
            self.changing
        }
    }

    fn last_line(out: &str) -> &str {
        out.lines().last().unwrap()
    }

    #[test]
    fn a_whole_machine_passes() {
        let (ok, out, lent) = Machine::new().run();
        assert!(ok, "{out}");
        assert!(
            last_line(&out).ends_with(
                "4 tables: 4 checked, 0 refused, 0 reported, 0 of them where the kernel relies on \
                 it, 0 with only their headers checked, 0 not readable, 0 too large to judge, 0 null \
                 XSDT entries, 0 lends over what the firmware changes; FADT true, DSDT true, FACS true; 0 us; \
                 consistent true"
            ),
            "{out}"
        );
        assert!(out.contains("FACS version 2, 64 bytes: checked"), "{out}");
        // TASKS item 71: what the first lookup located and recorded as never lent.
        assert!(
            out.contains(
                "never lent, as the firmware changes them after boot: FACS \
                 0x7f003000-0x7f00303f; 1 kept"
            ),
            "{out}"
        );
        // TASKS item 71: the PM timer's two forms, and the one the kernel's reader takes.
        assert!(
            out.contains(
                "PM timer X_PM_TMR_BLK zero, PM_TMR_BLK port 0x608; the kernel's: port 0x608"
            ),
            "{out}"
        );
        // The review's F2: the FACS is copied, never lent.
        assert!(!lent.contains(&FACS));
        // TASKS item 71: the guard checked live, a lend at the FACS's start and middle refused.
        assert!(
            out.contains("1 kept; a lend over each refused: true"),
            "{out}"
        );
        assert!(!lent.contains(&(FACS + 32)));
    }

    /// TASKS item 71: the census's live check of the guard. With it broken, the census counts
    /// each lend the guard would make over the FACS, its start, its middle and from before its
    /// start, and its verdict fails; and it lends none of them (the review's T71-7).
    #[test]
    fn a_lend_over_what_the_firmware_changes_fails_the_census() {
        let mut m = Machine::new();
        m.guard_broken = true;
        let (ok, out, lent) = m.run();
        assert!(!ok, "{out}");
        assert!(
            lent.iter().all(|&a| !(FACS - 8..FACS + 64).contains(&a)),
            "{lent:x?}"
        );
        assert!(
            out.contains(
                "1 kept; a lend over each refused: false (not over the FACS at \
                 0x7f003000-0x7f003023)"
            ),
            "{out}"
        );
        assert!(
            last_line(&out).contains(", 3 lends over what the firmware changes;"),
            "{out}"
        );
        assert!(last_line(&out).ends_with("consistent false: acpi tables failed"));
    }

    #[test]
    fn an_xsdt_entry_that_cannot_be_read_fails_the_census() {
        let mut m = Machine::new();
        m.entries.push(NOWHERE);
        let (ok, out, _) = m.run();
        assert!(!ok);
        assert!(out.contains("an XSDT entry: not readable"), "{out}");
        assert!(last_line(&out).ends_with("consistent false: acpi tables failed"));
    }

    #[test]
    fn a_null_entry_is_named_and_fails_nothing() {
        let mut m = Machine::new();
        m.entries.insert(0, 0);
        let (ok, out, _) = m.run();
        assert!(ok, "{out}");
        assert!(out.contains("XSDT entry 0: null, names no table"), "{out}");
        assert!(last_line(&out).contains(", 1 null XSDT entries,"), "{out}");
    }

    #[test]
    fn a_refused_table_the_kernel_does_not_use_fails_the_census() {
        let mut m = Machine::new();
        let mut oem = table(b"OEM1", 1, &[1, 2, 3]);
        oem[37] ^= 1;
        m.put(OEM, &oem);
        m.entries.push(OEM);
        let (ok, out, _) = m.run();
        assert!(!ok);
        assert!(out.contains("OEM1 rev 1, 39 bytes, OEM \"ORIGIN\": refused at 0x9: checksum"));
    }

    #[test]
    fn a_refused_or_unreadable_facs_fails_the_census() {
        let mut m = Machine::new();
        m.put(FACS, b"SCAF");
        let (ok, out, _) = m.run();
        assert!(!ok);
        assert!(
            out.contains("the FACS: refused at 0x0: signature SCAF"),
            "{out}"
        );
        assert!(last_line(&out).contains("FACS false"));

        let mut m = Machine::new();
        m.fadt(|f| {
            f[36..40].copy_from_slice(&0u32.to_le_bytes());
            f[132..140].copy_from_slice(&NOWHERE.to_le_bytes());
        });
        let (ok, out, _) = m.run();
        assert!(!ok);
        assert!(out.contains("the FACS: not readable"), "{out}");
    }

    #[test]
    fn a_facs_the_fadt_does_not_let_the_census_use_fails_it() {
        // The review's F3: a FACS not 64-byte aligned is reported, not used, and the FADT kept.
        let mut m = Machine::new();
        m.fadt(|f| f[36..40].copy_from_slice(&u32::try_from(FACS + 8).unwrap().to_le_bytes()));
        let (ok, out, _) = m.run();
        assert!(!ok);
        assert!(
            out.contains(
                "FACP rev 6, 276 bytes, OEM \"ORIGIN\": checked; 1 reported, at 0x24: \
                          FIRMWARE_CTRL: the FACS is 64-byte aligned"
            ),
            "{out}"
        );
        assert!(last_line(&out).contains("DSDT true, FACS false"), "{out}");
    }

    #[test]
    fn a_hardware_reduced_platform_needs_no_facs() {
        let mut m = Machine::new();
        m.fadt(|f| {
            f[36..40].copy_from_slice(&0u32.to_le_bytes());
            f[112..116].copy_from_slice(&(1u32 << 20).to_le_bytes());
        });
        let (ok, out, _) = m.run();
        assert!(ok, "{out}");
        assert!(last_line(&out).contains("FACS not needed"), "{out}");
        assert!(out.contains("hardware-reduced true"));
    }

    #[test]
    fn no_fadt_or_no_dsdt_fails_the_census() {
        let mut m = Machine::new();
        m.entries.clear();
        let (ok, out, _) = m.run();
        assert!(!ok);
        assert!(last_line(&out).contains("FADT false, DSDT false"), "{out}");

        let mut m = Machine::new();
        m.put(DSDT, &table(b"SSDT", 2, &[]));
        let (ok, out, _) = m.run();
        assert!(!ok);
        assert!(out.contains("refused at 0x0: signature SSDT where another was expected"));
    }

    #[test]
    fn the_fpdts_tables_are_copied_and_judged() {
        let pointer = |kind: u16, address: u64| {
            let mut r = kind.to_le_bytes().to_vec();
            r.extend_from_slice(&[16, 1, 0, 0, 0, 0]);
            r.extend_from_slice(&address.to_le_bytes());
            r
        };
        let fbpt = |boot_records: usize| {
            let mut t = b"FBPT".to_vec();
            t.extend_from_slice(&u32::try_from(8 + 48 * boot_records).unwrap().to_le_bytes());
            for _ in 0..boot_records {
                t.extend_from_slice(&[2, 0, 48, 2]);
                t.extend_from_slice(&[0; 44]);
            }
            t
        };
        let machine = |fbpt_bytes: &[u8], target: u64| {
            let mut m = Machine::new();
            m.put(FPDT, &table(b"FPDT", 1, &pointer(0, target)));
            m.put(FBPT, fbpt_bytes);
            m.entries.push(FPDT);
            m
        };
        let (ok, out, lent) = machine(&fbpt(1), FBPT).run();
        assert!(ok, "{out}");
        assert!(out.contains("FBPT, 56 bytes: checked: 1 records"), "{out}");
        assert!(!lent.contains(&FBPT), "the FBPT is copied, never lent (F2)");
        // Refused: a table it needs is wrong.
        let (ok, out, _) = machine(&fbpt(2), FBPT).run();
        assert!(!ok);
        assert!(out.contains("FBPT: refused at 0x8"), "{out}");
        // Not readable: reported, and fails nothing.
        let (ok, out, _) = machine(&fbpt(1), NOWHERE).run();
        assert!(ok, "{out}");
        assert!(out.contains("FBPT: not readable"), "{out}");
    }

    #[test]
    fn the_verdict_weighs_each_finding() {
        let good = Findings {
            fadt: true,
            dsdt: true,
            facs: true,
            reported: 3,
            null_entries: 2,
            unreadable: 1,
            ..Findings::default()
        };
        assert!(good.consistent());
        for bad in [
            Findings { refused: 1, ..good },
            Findings {
                unreadable_entries: 1,
                ..good
            },
            Findings {
                fadt: false,
                ..good
            },
            Findings {
                dsdt: false,
                ..good
            },
            Findings {
                facs: false,
                ..good
            },
        ] {
            assert!(!bad.consistent(), "{bad:?}");
        }
        assert!(
            Findings {
                facs: false,
                reduced: true,
                ..good
            }
            .consistent()
        );
    }

    /// A MADT with a local APIC address and `entries`.
    fn madt(entries: &[&[u8]]) -> Vec<u8> {
        let mut body = 0xFEE0_0000u32.to_le_bytes().to_vec();
        body.extend_from_slice(&[0; 4]);
        for e in entries {
            body.extend_from_slice(e);
        }
        table(b"APIC", 5, &body)
    }

    /// The review's addendum, N1: a fault in a structure the kernel relies on fails the verdict,
    /// though the table is not refused; one in a structure it does not use is only reported.
    #[test]
    fn a_fault_where_the_kernel_relies_fails_the_verdict() {
        let lapic: &[u8] = &[0, 8, 0, 0, 1, 0, 0, 0];
        let short_lapic: &[u8] = &[0, 7, 1, 1, 1, 0, 0];
        let bad_nmi: &[u8] = &[4, 6, 0xFF, 0, 0, 2];
        let run = |table: Vec<u8>| {
            let mut m = Machine::new();
            m.put(OEM, &table);
            m.entries.push(OEM);
            m.run()
        };
        let (ok, out, _) = run(madt(&[lapic, bad_nmi]));
        assert!(ok, "{out}");
        assert!(
            last_line(&out).contains("1 reported, 0 of them where the kernel relies on it"),
            "{out}"
        );
        let (ok, out, _) = run(madt(&[lapic, short_lapic]));
        assert!(!ok, "{out}");
        assert!(out.contains(
            "APIC rev 5, 59 bytes, OEM \"ORIGIN\": checked: 1 structures (0x1); 1 reported"
        ));
        assert!(
            last_line(&out)
                .contains("0 refused, 1 reported, 1 of them where the kernel relies on it"),
            "{out}"
        );
        assert!(last_line(&out).ends_with("consistent false: acpi tables failed"));
        // A second I/O APIC at one address is a fault in a structure the kernel reads.
        let ioapic = |id: u8, base: u32| {
            let mut e = vec![1u8, 12, id, 0];
            e.extend_from_slice(&0xFEC0_0000u32.to_le_bytes());
            e.extend_from_slice(&base.to_le_bytes());
            e
        };
        let (ok, _, _) = run(madt(&[lapic, &ioapic(1, 0), &ioapic(2, 24)]));
        assert!(!ok);
        // The SRAT: a memory range that wraps, enabled, fails it; disabled, it is ignored.
        let memory = |flags: u32| {
            let mut e = vec![1u8, 40, 0, 0, 0, 0, 0, 0];
            e.extend_from_slice(&(u64::MAX - 1).to_le_bytes());
            e.extend_from_slice(&2u64.to_le_bytes());
            e.extend_from_slice(&[0; 4]);
            e.extend_from_slice(&flags.to_le_bytes());
            e.extend_from_slice(&[0; 8]);
            e
        };
        let srat = |e: Vec<u8>| {
            let mut body = vec![1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            body.extend_from_slice(&e);
            table(b"SRAT", 3, &body)
        };
        assert!(!run(srat(memory(1))).0);
        assert!(run(srat(memory(0))).0);
    }

    /// N1 for the FADT: a reset register the flags say is used, of the wrong shape, fails the
    /// verdict; a sleep register of the wrong shape, which the kernel does not use, does not.
    #[test]
    fn a_fadt_field_the_kernel_relies_on_fails_the_verdict() {
        let gas = |space: u8, width: u8| {
            let mut g = vec![space, width, 0, 1];
            g.extend_from_slice(&0xCF9u64.to_le_bytes());
            g
        };
        let mut m = Machine::new();
        m.fadt(|f| {
            f[112..116].copy_from_slice(&(1u32 | 1 << 10).to_le_bytes());
            f[116..128].copy_from_slice(&gas(1, 16));
        });
        let (ok, out, _) = m.run();
        assert!(!ok, "{out}");
        assert!(
            out.contains("1 reported, at 0x74: RESET_REG holds a value it may not"),
            "{out}"
        );
        let mut m = Machine::new();
        m.fadt(|f| f[244..256].copy_from_slice(&gas(1, 16)));
        let (ok, out, _) = m.run();
        assert!(ok, "{out}");
        assert!(last_line(&out).contains("1 reported, 0 of them where the kernel relies on it"));
    }

    /// The review's addendum, N2: an FBPT whose length (the space the firmware set aside) is past
    /// 64 KiB is copied and judged up to 1 MiB; past that it is "too large to judge", never "not
    /// readable", and fails nothing.
    #[test]
    fn a_large_performance_table_is_judged_or_named_too_large() {
        let pointer = |address: u64| {
            let mut r = vec![0u8, 0, 16, 1, 0, 0, 0, 0];
            r.extend_from_slice(&address.to_le_bytes());
            r
        };
        let fbpt = |length: u32| {
            let mut t = b"FBPT".to_vec();
            t.extend_from_slice(&length.to_le_bytes());
            t.extend_from_slice(&[2, 0, 48, 2]);
            t.extend_from_slice(&[0; 44]);
            t
        };
        let run = |length: u32| {
            let mut m = Machine::new();
            m.put(FPDT, &table(b"FPDT", 1, &pointer(FBPT)));
            m.put(FBPT, &fbpt(length));
            m.entries.push(FPDT);
            m.run()
        };
        let (ok, out, _) = run(200 << 10);
        assert!(ok, "{out}");
        assert!(
            out.contains("FBPT, 204800 bytes: checked: 1 records"),
            "{out}"
        );
        let (ok, out, _) = run(2 << 20);
        assert!(ok, "{out}");
        assert!(
            out.contains("FBPT: 2097152 bytes, too large to judge"),
            "{out}"
        );
        assert!(last_line(&out).contains(", 1 too large to judge,"), "{out}");
    }

    /// The review's addendum, N3: a structure the firmware changes after boot is copied and never
    /// lent, wherever it is named from: an XSDT entry that names the FACS gets it judged as a FACS;
    /// a DSDT pointer that names it gets a refusal, without lending it.
    #[test]
    fn a_changing_structure_is_never_lent_wherever_it_is_named() {
        let mut m = Machine::new();
        m.entries.push(FACS);
        let (ok, out, lent) = m.run();
        assert!(ok, "{out}");
        assert!(
            out.contains("an XSDT entry: FACS, 64 bytes, copied, never lent: checked"),
            "{out}"
        );
        assert!(!lent.contains(&FACS), "{lent:x?}");
        let mut m = Machine::new();
        m.fadt(|f| {
            f[40..44].copy_from_slice(&u32::try_from(FACS).unwrap().to_le_bytes());
            f[140..148].copy_from_slice(&FACS.to_le_bytes());
        });
        let (ok, out, lent) = m.run();
        assert!(!ok);
        assert!(
            out.contains(
                "the DSDT: FACS, 64 bytes, copied, never lent: refused at 0x0: signature FACS \
                 where another was expected"
            ),
            "{out}"
        );
        assert!(!lent.contains(&FACS), "{lent:x?}");
    }
}
