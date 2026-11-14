//! The census: any table the firmware lists, checked as far as this crate knows it, so that every
//! entry of the XSDT is accounted for (ADR-0019 A6, area A; TASKS item 62).
//!
//! A table of ACPI 6.6 §5.2 that a platform in Origin's scope can present is parsed whole: every
//! structure, nested list and reference in it. Any other table has its header, length and
//! checksum checked and is listed by whose it is: chapters 14 and 18 of ACPI 6.6 (areas H and G of
//! ADR-0019 A6), a specification Table 5.6 of §5.2.6 names, a §5.2 table for other processors
//! (ADR-0019 A1), the OEM's, or a signature no document named here defines. What those documents
//! define is "not supported" until a task reads them.

use crate::error::{Error, Signature};
use crate::header::{self, Header, Table};

/// One structure, or one run of bytes a parser hands on (a definition block's AML, a name), as
/// the census met it: for tests, which check that each lies inside its table.
#[derive(Clone, Copy, Debug)]
pub struct Seen<'a> {
    /// Its offset from the table's first byte.
    pub at: usize,
    /// Its type, or [`Seen::BYTES`] for a run of bytes that is not a typed structure.
    pub kind: u16,
    /// Its bytes.
    pub bytes: &'a [u8],
}

impl Seen<'_> {
    /// The kind of a run of bytes handed on that is not a typed structure.
    pub const BYTES: u16 = u16::MAX;
}

/// How many distinct structure types a summary counts one by one; the rest are counted together.
const KINDS: usize = 24;

/// How many of a table's reported faults a summary keeps; the rest are only counted.
pub const NOTED: usize = 4;

/// What a parsed table held: its structures, counted by type, and what it reported.
///
/// A structure or field that breaks a rule of §5.2 but does not stop the walk (its length still
/// locates what follows) is *reported*: counted here, its first [`NOTED`] faults kept, and not
/// used. Only a fault that stops the walk, or that leaves the table unusable as a whole, refuses
/// the table (the independent review's F3 and F5, and the coordinating session's rulings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    structures: u32,
    entries: u32,
    kinds: [(u16, u32); KINDS],
    distinct: usize,
    others: u32,
    reported: u32,
    notes: [Option<Error>; NOTED],
}

impl Summary {
    pub(crate) const fn new() -> Self {
        Self {
            structures: 0,
            entries: 0,
            kinds: [(0, 0); KINDS],
            distinct: 0,
            others: 0,
            reported: 0,
            notes: [None; NOTED],
        }
    }

    /// Records a reported fault.
    pub(crate) fn report(&mut self, fault: Error) {
        if let Some(slot) = self.notes.iter_mut().find(|n| n.is_none()) {
            *slot = Some(fault);
        }
        self.reported = self.reported.saturating_add(1);
    }

    /// How many faults the table reported.
    #[must_use]
    pub const fn reported(&self) -> u32 {
        self.reported
    }

    /// The first reported faults, up to [`NOTED`].
    pub fn notes(&self) -> impl Iterator<Item = Error> + '_ {
        self.notes.iter().flatten().copied()
    }

    /// Counts one structure of type `kind`.
    pub(crate) fn count(&mut self, kind: u16) {
        self.structures = self.structures.saturating_add(1);
        let seen = self.kinds.get_mut(..self.distinct).unwrap_or(&mut []);
        if let Some(entry) = seen.iter_mut().find(|(k, _)| *k == kind) {
            entry.1 = entry.1.saturating_add(1);
            return;
        }
        if let Some(slot) = self.kinds.get_mut(self.distinct) {
            *slot = (kind, 1);
            self.distinct = self.distinct.saturating_add(1);
        } else {
            self.others = self.others.saturating_add(1);
        }
    }

    /// Counts one entry of a table whose body is a list of plain values (the root tables'
    /// addresses, the PDTT's triggers, the SVKL's keys), which have no type of their own.
    pub(crate) fn entry(&mut self) {
        self.entries = self.entries.saturating_add(1);
    }

    /// How many plain entries the table held.
    #[must_use]
    pub const fn entries(&self) -> u32 {
        self.entries
    }

    /// How many structures the table held.
    #[must_use]
    pub const fn structures(&self) -> u32 {
        self.structures
    }

    /// Each type met and how many of it, in the order first met.
    pub fn kinds(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.kinds.iter().take(self.distinct).copied()
    }

    /// How many structures had a type past the ones counted one by one.
    #[must_use]
    pub const fn others(&self) -> u32 {
        self.others
    }

    /// How many structures of type `kind` it held.
    #[must_use]
    pub fn of_kind(&self, kind: u16) -> u32 {
        self.kinds
            .get(..self.distinct)
            .unwrap_or(&[])
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or(0, |(_, n)| *n)
    }
}

/// Counts what a parser meets and shows it to the caller's visitor.
pub(crate) struct Tally<'v, 'a> {
    pub(crate) summary: Summary,
    visit: &'v mut dyn FnMut(Seen<'a>),
}

impl<'v, 'a> Tally<'v, 'a> {
    pub(crate) fn new(visit: &'v mut dyn FnMut(Seen<'a>)) -> Self {
        Self {
            summary: Summary::new(),
            visit,
        }
    }

    /// A structure of type `kind` at `at`.
    pub(crate) fn structure(&mut self, kind: u16, at: usize, bytes: &'a [u8]) {
        self.summary.count(kind);
        (self.visit)(Seen { at, kind, bytes });
    }

    /// A structure that breaks a rule but does not stop the walk: reported and not used, its bytes
    /// still shown to the visitor.
    pub(crate) fn reported(&mut self, fault: Error, at: usize, bytes: &'a [u8]) {
        self.summary.report(fault);
        (self.visit)(Seen {
            at,
            kind: Seen::BYTES,
            bytes,
        });
    }

    /// A fault of a field that leaves the rest of the table usable (the review's F3).
    pub(crate) fn fault(&mut self, fault: Error) {
        self.summary.report(fault);
    }

    /// A run of bytes handed on, at `at`.
    pub(crate) fn bytes(&mut self, at: usize, bytes: &'a [u8]) {
        (self.visit)(Seen {
            at,
            kind: Seen::BYTES,
            bytes,
        });
    }
}

/// Whose a table is, and what the census did with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "one census is returned per table and read at once; the crate has no heap to box it in"
)]
pub enum Class {
    /// A table of ACPI 6.6 §5.2 this crate parses: every structure in it checked.
    Checked(Summary),
    /// A table of ACPI 6.6 that another area of ADR-0019 A6 builds; its header checked.
    OtherArea(&'static str),
    /// A table another specification defines (§5.2.6, Table 5.6); its header checked.
    OtherSpecification(&'static str),
    /// A §5.2 table for processors other than x86-64 (ADR-0019 A1); its header checked.
    NotX86(&'static str),
    /// An OEM table (a signature beginning `OEM`); its header checked.
    Oem,
    /// A signature none of those documents defines; its header checked.
    Unknown,
}

/// One table's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Census {
    /// Its checked header.
    pub header: Header,
    /// Whose it is, and what was found in it.
    pub class: Class,
}

/// The tables of ACPI 6.6 outside §5.2: chapter 18's error interfaces and chapter 14's channels.
const OTHER_AREAS: &[(&[u8; 4], &str)] = &[
    (b"BERT", "ACPI 6.6 §18.3.1, APEI (ADR-0019 area G)"),
    (b"EINJ", "ACPI 6.6 §18.6.1, APEI (ADR-0019 area G)"),
    (b"ERST", "ACPI 6.6 §18.5, APEI (ADR-0019 area G)"),
    (b"HEST", "ACPI 6.6 §18.3.2, APEI (ADR-0019 area G)"),
    (b"PCCT", "ACPI 6.6 §14.1, the PCC (ADR-0019 area H)"),
];

/// §5.2's tables for other processors (ADR-0019 A1).
const NOT_X86: &[(&[u8; 4], &str)] = &[
    (b"GTDT", "ACPI 6.6 §5.2.25, Arm's generic timers"),
    (b"RHCT", "ACPI 6.6 §5.2.37, RISC-V's hart capabilities"),
];

/// The signatures §5.2.6's Table 5.6 reserves for tables other specifications define.
const OTHER_SPECIFICATIONS: &[&[u8; 4]] = &[
    b"AEST", b"AGDI", b"APMT", b"ASPT", b"BDAT", b"BOOT", b"CEDT", b"CSRT", b"DBGP", b"DBG2",
    b"DMAR", b"DRTM", b"DTPR", b"ETDT", b"HPET", b"IBFT", b"IERS", b"IORT", b"IOVT", b"IRDT",
    b"IVRS", b"KEYP", b"LPIT", b"MCFG", b"MCHI", b"MHSP", b"MPAM", b"MSDM", b"NBFT", b"PRMT",
    b"RGRT", b"RIMT", b"RQSC", b"SDEI", b"SLIC", b"SPCR", b"SPMI", b"STAO", b"SWFT", b"TCPA",
    b"TPM2", b"UEFI", b"WAET", b"WDAT", b"WDDT", b"WDRT", b"WPBT", b"WSMT", b"XENV",
];

/// Whose a table that this crate does not parse is.
fn classify(signature: Signature) -> Class {
    if let Some((_, what)) = OTHER_AREAS.iter().find(|(s, _)| signature.is(s)) {
        return Class::OtherArea(what);
    }
    if let Some((_, what)) = NOT_X86.iter().find(|(s, _)| signature.is(s)) {
        return Class::NotX86(what);
    }
    if let Some(s) = OTHER_SPECIFICATIONS.iter().find(|s| signature.is(s)) {
        // The signature names its own specification: "DMAR's table".
        let _ = s;
        return Class::OtherSpecification(other_name(signature));
    }
    if signature.0.starts_with(b"OEM") {
        return Class::Oem;
    }
    Class::Unknown
}

/// The name a census line gives another specification's table: its signature, as a static string
/// (the table of names above is what makes it static).
fn other_name(signature: Signature) -> &'static str {
    OTHER_NAMES
        .iter()
        .find(|(s, _)| signature.is(s))
        .map_or("another specification", |(_, name)| *name)
}

/// What Table 5.6 says each signature is, shortened.
const OTHER_NAMES: &[(&[u8; 4], &str)] = &[
    (b"AEST", "Arm's error source table"),
    (b"AGDI", "Arm's diagnostic dump and reset interface"),
    (b"APMT", "Arm's performance monitoring unit table"),
    (b"ASPT", "AMD's secure processor table"),
    (b"BDAT", "the BIOS data table"),
    (b"BOOT", "a reserved signature"),
    (b"CEDT", "CXL's early discovery table"),
    (b"CSRT", "the core system resource table"),
    (b"DBGP", "the debug port table"),
    (b"DBG2", "the debug port table 2"),
    (b"DMAR", "Intel VT-d's DMA remapping table"),
    (b"DRTM", "TCG's D-RTM table"),
    (b"DTPR", "Intel TXT's DMA protection ranges"),
    (b"ETDT", "the obsolete event timer table"),
    (b"HPET", "the IA-PC HPET table"),
    (b"IBFT", "the iSCSI boot firmware table"),
    (b"IERS", "the inline encryption reporting structure"),
    (b"IORT", "Arm's I/O remapping table"),
    (b"IOVT", "the I/O virtualization table"),
    (b"IRDT", "the I/O resource director technology table"),
    (b"IVRS", "AMD-Vi's I/O virtualization reporting structure"),
    (b"KEYP", "the root complex IDE key programming table"),
    (b"LPIT", "the low power idle table"),
    (b"MCFG", "PCI Firmware's memory-mapped configuration table"),
    (b"MCHI", "DMTF's management controller host interface table"),
    (b"MHSP", "Microsoft's Pluton table"),
    (b"MPAM", "Arm's memory partitioning and monitoring table"),
    (b"MSDM", "Microsoft's data management table"),
    (b"NBFT", "the NVMe-oF boot firmware table"),
    (b"PRMT", "the platform runtime mechanism table"),
    (b"RGRT", "the regulatory graphics resource table"),
    (b"RIMT", "RISC-V's I/O mapping table"),
    (b"RQSC", "RISC-V's QoS controllers table"),
    (b"SDEI", "Arm's software delegated exceptions interface"),
    (b"SLIC", "Microsoft's software licensing table"),
    (b"SPCR", "Microsoft's serial port console redirection table"),
    (b"SPMI", "the server platform management interface table"),
    (b"STAO", "the _STA override table"),
    (b"SWFT", "the SoundWire file table"),
    (b"TCPA", "TCG's TCPA table"),
    (b"TPM2", "TCG's TPM 2 table"),
    (b"UEFI", "UEFI's table"),
    (b"WAET", "Microsoft's emulated devices table"),
    (b"WDAT", "Microsoft's watchdog action table"),
    (b"WDDT", "the watchdog descriptor table"),
    (b"WDRT", "the watchdog resource table"),
    (b"WPBT", "Microsoft's platform binary table"),
    (b"WSMT", "Microsoft's SMM security mitigations table"),
    (b"XENV", "the Xen project's table"),
];

/// Checks any table that begins with a system description table header: its header, length and
/// checksum, and, for a §5.2 table this crate parses, every structure in it. `visit` sees every
/// structure and every run of bytes a parser hands on.
pub fn check_with<'a>(bytes: &'a [u8], visit: &mut dyn FnMut(Seen<'a>)) -> Result<Census, Error> {
    let table = header::check(bytes)?;
    let mut tally = Tally::new(visit);
    let parsed = parse(&table, &mut tally)?;
    let class = if parsed {
        Class::Checked(tally.summary)
    } else {
        classify(table.signature())
    };
    Ok(Census {
        header: *table.header(),
        class,
    })
}

/// [`check_with`], with no visitor.
pub fn check(bytes: &[u8]) -> Result<Census, Error> {
    check_with(bytes, &mut |_| {})
}

/// Runs the parser for `table`'s signature over the whole of it. `Ok(false)` if this crate has
/// none.
fn parse<'a>(table: &Table<'a>, tally: &mut Tally<'_, 'a>) -> Result<bool, Error> {
    let bytes = table.bytes();
    match &table.signature().0 {
        b"APIC" => crate::madt::Madt::parse(bytes)?.census(tally)?,
        b"FACP" => {
            for fault in crate::fadt::Fadt::parse(bytes)?.faults() {
                tally.fault(fault);
            }
        }
        b"DSDT" | b"SSDT" | b"PSDT" => {
            let block = crate::definition::DefinitionBlock::parse(bytes)?;
            tally.bytes(header::HEADER_LEN, block.aml());
        }
        b"SRAT" => crate::srat::Srat::parse(bytes)?.census(tally)?,
        b"SLIT" => {
            let slit = crate::platform::Slit::parse(bytes)?;
            let _ = slit.localities();
        }
        b"SBST" => {
            crate::platform::Sbst::parse(bytes)?;
        }
        b"ECDT" => {
            let ecdt = crate::platform::Ecdt::parse(bytes)?;
            tally.bytes(65, ecdt.id);
        }
        b"CPEP" => crate::platform::Cpep::parse(bytes)?.census(tally)?,
        b"MSCT" => crate::platform::Msct::parse(bytes)?.census(tally)?,
        b"RASF" => {
            crate::ras::Rasf::parse(bytes)?;
        }
        b"RAS2" => crate::ras::Ras2::parse(bytes)?.census(tally)?,
        b"MPST" => crate::memory::Mpst::parse(bytes)?.census(tally)?,
        b"PMTT" => crate::memory::Pmtt::parse(bytes)?.census(tally)?,
        b"BGRT" => {
            crate::boot::Bgrt::parse(bytes)?;
        }
        b"FPDT" => crate::boot::Fpdt::parse(bytes)?.census(tally)?,
        b"NFIT" => crate::nfit::Nfit::parse(bytes)?.census(tally)?,
        b"NHLT" => crate::nhlt::Nhlt::parse(bytes)?.census(tally)?,
        b"SDEV" => crate::sdev::Sdev::parse(bytes)?.census(tally)?,
        b"HMAT" => crate::hmat::Hmat::parse(bytes)?.census(tally)?,
        b"PPTT" => crate::pptt::Pptt::parse(bytes)?.census(tally)?,
        b"VIOT" => crate::viot::Viot::parse(bytes)?.census(tally)?,
        b"PHAT" => crate::records::Phat::parse(bytes)?.census(tally)?,
        b"MISC" => crate::records::Misc::parse(bytes)?.census(tally)?,
        b"PDTT" => {
            for _ in crate::records::Pdtt::parse(bytes)?.triggers() {
                tally.summary.entry();
            }
        }
        b"CCEL" => {
            crate::records::Ccel::parse(bytes)?;
        }
        b"SVKL" => {
            for _ in crate::records::Svkl::parse(bytes)?.keys() {
                tally.summary.entry();
            }
        }
        b"RSDT" | b"XSDT" => {
            for _ in crate::root::Root::parse(bytes)?.entries() {
                tally.summary.entry();
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::table;

    #[test]
    fn every_signature_is_accounted_for() {
        for (sig, expect) in [
            (
                b"BERT",
                Class::OtherArea("ACPI 6.6 §18.3.1, APEI (ADR-0019 area G)"),
            ),
            (
                b"PCCT",
                Class::OtherArea("ACPI 6.6 §14.1, the PCC (ADR-0019 area H)"),
            ),
            (
                b"GTDT",
                Class::NotX86("ACPI 6.6 §5.2.25, Arm's generic timers"),
            ),
            (
                b"DMAR",
                Class::OtherSpecification("Intel VT-d's DMA remapping table"),
            ),
            (
                b"WAET",
                Class::OtherSpecification("Microsoft's emulated devices table"),
            ),
            (b"OEMB", Class::Oem),
            (b"ZZZZ", Class::Unknown),
        ] {
            let t = table(sig, 1, &[]);
            assert_eq!(check(&t).unwrap().class, expect, "{sig:?}");
        }
        assert_eq!(OTHER_SPECIFICATIONS.len(), OTHER_NAMES.len());
        for s in OTHER_SPECIFICATIONS {
            assert_ne!(other_name(Signature::new(s)), "another specification");
        }
    }

    #[test]
    fn a_summary_counts_by_kind() {
        let mut s = Summary::new();
        for kind in [0u16, 1, 0, 9] {
            s.count(kind);
        }
        assert_eq!(s.structures(), 4);
        assert_eq!(s.of_kind(0), 2);
        assert_eq!(s.of_kind(5), 0);
        assert_eq!(s.kinds().collect::<Vec<_>>(), [(0, 2), (1, 1), (9, 1)]);
        let mut many = Summary::new();
        for kind in 0..30u16 {
            many.count(kind);
        }
        assert_eq!(many.kinds().count(), KINDS);
        assert_eq!(many.others(), 6);
        let mut entries = Summary::new();
        entries.entry();
        entries.entry();
        assert_eq!((entries.entries(), entries.structures()), (2, 0));
    }
}
