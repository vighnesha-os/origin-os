//! How the census of ACPI tables (`acpi_census`; TASKS item 62) words what `origin-acpi-tables`
//! found. The words live here, in the kernel, and not in the crate: the crate is one of the
//! freestanding libraries whose release objects may reference no formatting beyond
//! `Formatter::write_str` (the no-panic check, ONBF §12), and numbers need `core::fmt`.

use core::fmt;

use origin_acpi_tables::census::{Class, Summary};
use origin_acpi_tables::{Error, ErrorKind, Signature};

/// A signature: printable ASCII as itself, anything else as `?`.
#[derive(Debug)]
pub struct Sig(pub Signature);

impl fmt::Display for Sig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0.printable() {
            fmt::Write::write_char(f, char::from(b))?;
        }
        Ok(())
    }
}

/// Firmware text (an OEM ID, say): printable ASCII as itself, anything else as `?`.
#[derive(Debug)]
pub struct Ascii<'a>(pub &'a [u8]);

impl fmt::Display for Ascii<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in self.0 {
            let c = if b.is_ascii_graphic() || b == b' ' {
                char::from(b)
            } else {
                '?'
            };
            fmt::Write::write_char(f, c)?;
        }
        Ok(())
    }
}

/// The rule a refusal names.
#[derive(Debug)]
pub struct Kind(pub ErrorKind);

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ErrorKind::Signature { found } => {
                write!(f, "signature {} where another was expected", Sig(found))
            }
            ErrorKind::Length {
                declared,
                available,
            } => write!(f, "length {declared} with {available} bytes given"),
            ErrorKind::Revision(r) => write!(f, "revision {r}, whose layout is not ACPI 6.6's"),
            ErrorKind::TooShort { need } => {
                write!(f, "too short for its revision: needs {need} bytes")
            }
            ErrorKind::Checksum => f.write_str("checksum"),
            ErrorKind::Truncated { need } => write!(f, "runs past the end, to {need:#x}"),
            ErrorKind::Structure { kind, length } => {
                write!(f, "structure type {kind} with length {length}")
            }
            ErrorKind::Offset { field } => write!(f, "{field} points outside its table"),
            ErrorKind::Value { field } => write!(f, "{field} holds a value it may not"),
            ErrorKind::Inconsistent { field } => write!(f, "{field} disagrees with another field"),
            ErrorKind::Reference { field } => write!(f, "{field} names no structure of its kind"),
            ErrorKind::Cycle => f.write_str("its parents form a cycle"),
        }
    }
}

/// A refusal, as a census line gives it: where, and which rule.
#[derive(Debug)]
pub struct Refused(pub Error);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refused at {:#x}: {}", self.0.at, Kind(self.0.kind))
    }
}

/// What a parsed table held: `N entries`, `N structures (type×count, ...)`, both, or nothing.
#[derive(Debug)]
pub struct Counted(pub Summary);

impl fmt::Display for Counted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = &self.0;
        if s.entries() != 0 {
            write!(f, "{} entries", s.entries())?;
            if s.structures() != 0 {
                f.write_str(", ")?;
            }
        }
        if s.structures() == 0 {
            return Ok(());
        }
        write!(f, "{} structures (", s.structures())?;
        for (i, (kind, n)) in s.kinds().enumerate() {
            if i != 0 {
                f.write_str(", ")?;
            }
            write!(f, "{kind}x{n}")?;
        }
        if s.others() != 0 {
            write!(f, ", {} more", s.others())?;
        }
        f.write_str(")")
    }
}

/// What a checked table reported and did not use (the review's F3 and F5): nothing, or `; N
/// reported, at 0x..: rule, ...`, the first few by where and which rule.
#[derive(Debug)]
pub struct Noted(pub Summary);

impl fmt::Display for Noted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = &self.0;
        if s.reported() == 0 {
            return Ok(());
        }
        write!(f, "; {} reported", s.reported())?;
        let mut shown = 0u32;
        for e in s.notes() {
            write!(f, ", at {:#x}: {}", e.at, Kind(e.kind))?;
            shown = shown.saturating_add(1);
        }
        if s.reported() > shown {
            write!(f, ", and {} more", s.reported().saturating_sub(shown))?;
        }
        Ok(())
    }
}

/// Whose a table is, and what the census did with it.
#[derive(Debug)]
pub struct Whose(pub Class);

impl fmt::Display for Whose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Class::Checked(s) if s.structures() == 0 && s.entries() == 0 => {
                write!(f, "checked{}", Noted(s))
            }
            Class::Checked(s) => write!(f, "checked: {}{}", Counted(s), Noted(s)),
            Class::OtherArea(what) => write!(f, "{what}: header checked, not parsed here"),
            Class::OtherSpecification(what) => {
                write!(f, "{what}, another specification's: header checked")
            }
            Class::NotX86(what) => write!(f, "{what}: not for x86-64, header checked"),
            Class::Oem => f.write_str("an OEM's table: header checked"),
            Class::Unknown => f.write_str("a signature not known here: header checked"),
            Class::Unsupported(what) => {
                write!(
                    f,
                    "{what}, a layout ACPI 6.6 no longer gives: header checked, not supported"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use origin_acpi_tables::census;

    /// A table with `signature` and `body`, its length and checksum set.
    fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[..4].copy_from_slice(signature);
        t[8] = 1;
        t.extend_from_slice(body);
        let len = u32::try_from(t.len()).unwrap();
        t[4..8].copy_from_slice(&len.to_le_bytes());
        let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[9] = 0u8.wrapping_sub(sum);
        t
    }

    fn line(t: &[u8]) -> String {
        match census::check(t) {
            Ok(c) => format!("{}", Whose(c.class)),
            Err(e) => format!("{}", Refused(e)),
        }
    }

    #[test]
    fn the_census_lines_read_as_the_boot_prints_them() {
        let mut xsdt = Vec::new();
        for a in [0x1000u64, 0x2000] {
            xsdt.extend_from_slice(&a.to_le_bytes());
        }
        assert_eq!(line(&table(b"XSDT", &xsdt)), "checked: 2 entries");
        assert_eq!(line(&table(b"SBST", &[0; 12])), "checked");
        let mut madt = vec![0u8; 8];
        madt.extend_from_slice(&[0, 8, 0, 0, 1, 0, 0, 0, 0, 8, 1, 1, 1, 0, 0, 0, 1, 12]);
        madt.extend_from_slice(&[0; 10]);
        assert_eq!(
            line(&table(b"APIC", &madt)),
            "checked: 3 structures (0x2, 1x1)"
        );
        assert_eq!(
            line(&table(b"HPET", &[])),
            "the IA-PC HPET table, another specification's: header checked"
        );
        assert_eq!(
            line(&table(b"BERT", &[])),
            "ACPI 6.6 §18.3.1, APEI (ADR-0019 area G): header checked, not parsed here"
        );
        assert_eq!(
            line(&table(b"GTDT", &[])),
            "ACPI 6.6 §5.2.25, Arm's generic timers: not for x86-64, header checked"
        );
        assert_eq!(line(&table(b"OEM1", &[])), "an OEM's table: header checked");
        assert_eq!(
            line(&table(b"Q\x01RS", &[])),
            "a signature not known here: header checked"
        );
        assert_eq!(
            line(&table(b"PMTT", &[0; 4])),
            "the PMTT's revision 1, deprecated since ACPI 6.4, a layout ACPI 6.6 no longer gives: \
             header checked, not supported"
        );
        let mut bad = table(b"SBST", &[0; 12]);
        bad[40] ^= 1;
        assert_eq!(line(&bad), "refused at 0x9: checksum");
        assert_eq!(
            line(&table(b"APIC", &[0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0])),
            "refused at 0x2c: structure type 128 with length 0"
        );
        // A structure that breaks a rule is reported and not used; the table is still checked.
        let mut nmi = vec![0u8; 8];
        nmi.extend_from_slice(&[4, 6, 0xFF, 0, 0, 2, 0, 8, 0, 0, 1, 0, 0, 0]);
        assert_eq!(
            line(&table(b"APIC", &nmi)),
            "checked: 1 structures (0x1); 1 reported, at 0x31: LINT# holds a value it may not"
        );
        let mut many = vec![0u8; 8];
        for _ in 0..6 {
            many.extend_from_slice(&[4, 6, 0xFF, 0, 0, 2]);
        }
        assert_eq!(
            line(&table(b"APIC", &many)),
            "checked; 6 reported, at 0x31: LINT# holds a value it may not, at 0x37: LINT# holds a \
             value it may not, at 0x3d: LINT# holds a value it may not, at 0x43: LINT# holds a \
             value it may not, and 2 more"
        );
        assert_eq!(format!("{}", Sig(Signature::new(b"A\x07\xffZ"))), "A??Z");
        assert_eq!(format!("{}", Ascii(b"BOCHS ")), "BOCHS ");
    }

    #[test]
    fn every_kind_of_refusal_has_its_words() {
        for kind in [
            ErrorKind::Signature {
                found: Signature::new(b"SSDT"),
            },
            ErrorKind::Length {
                declared: 40,
                available: 36,
            },
            ErrorKind::Revision(1),
            ErrorKind::TooShort { need: 44 },
            ErrorKind::Checksum,
            ErrorKind::Truncated { need: 0x30 },
            ErrorKind::Structure { kind: 1, length: 3 },
            ErrorKind::Offset {
                field: "Node Offset",
            },
            ErrorKind::Value { field: "Bus" },
            ErrorKind::Inconsistent { field: "DAY_ALRM" },
            ErrorKind::Reference { field: "Parent" },
            ErrorKind::Cycle,
        ] {
            let text = format!("{}", Kind(kind));
            assert!(!text.is_empty() && !text.contains('{'), "{kind:?}: {text}");
        }
    }
}
