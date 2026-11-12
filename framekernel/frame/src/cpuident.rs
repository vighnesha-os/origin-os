//! The CPU identification table (ADR-0009 Amendment 1, A2; CLAUDE.md §24; TASKS item 16).
//!
//! CPUID names a processor by its maker (leaf 0) and by a family, a model and a stepping (leaf
//! 1, EAX). This file turns those into a core generation and says whether that generation is
//! in the declared scope (ADR-0009 A1). It also holds the two other things the kernel chooses
//! from a maker, a family or a model rather than from a feature bit: the form
//! [`crate::nospec`] uses on a wrongly guessed path (ADR-0017 Z9, a column of the table), and
//! errata ([`ERRATA`]).
//!
//! What it decides, and what it does not:
//!
//! - **The scope line the boot prints** ([`Identity`]'s `Display`). The kernel never refuses
//!   to boot on it, and no proof's verdict counts it (A2; ADR-0013 R3).
//! - **No instruction is chosen from it.** Features come from feature bits (`features`,
//!   `speculation`; CLAUDE.md §14, §24).
//! - **It is not a security control.** A hypervisor can report any identity, and QEMU can be
//!   told to (A2). The line says when a hypervisor runs the processor.
//! - **A machine it cannot place is outside the scope.** Only Intel's and AMD's own
//!   publications put a row in the scope, and only where A2's rule does.
//!
//! Every row names the vendor documents it rests on ([`Document`]). Their numbers, revisions,
//! dates and hashes are in `docs/research/2026-11-12-cpu-identification-table.md`, read on
//! that day. No operating system's tables were read for it (CLAUDE.md §3).

#![forbid(unsafe_code)]

use core::fmt;

use crate::nospec::Form;

/// Who made the processor, from the twelve letters of CPUID leaf 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    /// "GenuineIntel".
    Intel,
    /// "AuthenticAMD".
    Amd,
    /// Anything else.
    Other,
}

/// The twelve letters of CPUID leaf 0, in the order the registers hold them: EBX, EDX, ECX.
#[must_use]
pub const fn vendor_letters(leaf0: [u32; 4]) -> [u8; 12] {
    let (b, d, c) = (
        leaf0[1].to_le_bytes(),
        leaf0[3].to_le_bytes(),
        leaf0[2].to_le_bytes(),
    );
    [
        b[0], b[1], b[2], b[3], d[0], d[1], d[2], d[3], c[0], c[1], c[2], c[3],
    ]
}

impl Maker {
    /// The maker CPUID leaf 0 names.
    #[must_use]
    pub fn of(leaf0: [u32; 4]) -> Self {
        match &vendor_letters(leaf0) {
            b"GenuineIntel" => Self::Intel,
            b"AuthenticAMD" => Self::Amd,
            _ => Self::Other,
        }
    }
}

/// The family in leaf 1 EAX. Intel and AMD form it the same way: the base family (bits 11:8),
/// plus the extended family (bits 27:20) where the base family is 0FH (Intel SDM Vol. 1
/// §21.1.2; AMD APM Vol. 3 Appendix E.3.2).
#[must_use]
pub const fn family_of(eax: u32) -> u32 {
    let base = (eax >> 8) & 0xF;
    if base == 0xF {
        base + ((eax >> 20) & 0xFF)
    } else {
        base
    }
}

/// What CPUID says the processor is: leaf 0's maker and leaf 1's signature, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    /// The maker.
    pub maker: Maker,
    /// Leaf 0's twelve letters, as the processor gave them.
    pub vendor: [u8; 12],
    /// Leaf 1 EAX, as read.
    pub eax: u32,
    /// The family ([`family_of`]).
    pub family: u32,
    /// The model, by the maker's own rule: Intel puts the extended model (bits 19:16) above
    /// the base model (bits 7:4) where the base family is 06H or 0FH; AMD only where it is
    /// 0FH. Another maker's is decoded by Intel's rule; the line prints EAX beside it.
    pub model: u32,
    /// Bits 3:0.
    pub stepping: u32,
    /// Leaf 1 ECX bit 31: a hypervisor runs this processor.
    pub hosted: bool,
}

impl Signature {
    /// Decodes leaf 0 and leaf 1's EAX and ECX.
    #[must_use]
    pub fn new(leaf0: [u32; 4], eax: u32, ecx: u32) -> Self {
        let maker = Maker::of(leaf0);
        let base_family = (eax >> 8) & 0xF;
        let extended = match maker {
            Maker::Amd => base_family == 0xF,
            Maker::Intel | Maker::Other => base_family == 0x6 || base_family == 0xF,
        };
        let base_model = (eax >> 4) & 0xF;
        let model = if extended {
            ((eax >> 16) & 0xF) << 4 | base_model
        } else {
            base_model
        };
        Self {
            maker,
            vendor: vendor_letters(leaf0),
            eax,
            family: family_of(eax),
            model,
            stepping: eax & 0xF,
            hosted: ecx & (1 << 31) != 0,
        }
    }
}

/// A vendor publication a row rests on. The ledger has each one's revision, date and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Document {
    /// Intel SDM Volume 4 (335592-093, September 2026), Table 2-1.
    IntelSdmVol4,
    /// Intel's Instruction Set Extensions and Future Features Programming Reference
    /// (319433-062, June 2026), Table 1-1.
    IntelFutureFeatures,
    /// Intel's perfmon `mapfile.csv` (`intel/perfmon`, the file at `409ce6f`, 2026-10-13).
    IntelPerfmon,
    /// Intel's microcode release notes (`microcode-20261110`).
    IntelMicrocode,
    /// AMD APM Volume 3 (24594, revision 3.39, October 2026), Appendix E.3.2.
    AmdApmVol3,
    /// AMD's revision guide for family 1AH models 00H-0FH (58251, revision 1.30): "Turin-Zen5".
    AmdRevisionGuide58251,
    /// AMD's revision guide for family 1AH models 10H-1FH (58730, revision 1.30): "Turin-Zen5c".
    AmdRevisionGuide58730,
    /// AMD's revision guide for family 19H models 10H-1FH (57095, revision 1.05): EPYC 9004.
    AmdRevisionGuide57095,
    /// AMD's revision guide for family 19H models A0H-AFH (57926, revision 1.05): "Zen4c".
    AmdRevisionGuide57926,
    /// AMD's revision guide for family 17H models 30H-3FH (56323, revision 1.03): EPYC 7002.
    AmdRevisionGuide56323,
    /// AMD's revision guide for family 17H models 00H-0FH (55449, revision 1.21): EPYC.
    AmdRevisionGuide55449,
    /// AMD's processor programming references for family 1AH models 50H (58264) and C0H
    /// (58655), both dated 2026-09-30: family 1AH goes on into 2026.
    AmdPpr2026,
}

impl Document {
    /// The document's number and revision, as the ledger cites it.
    #[must_use]
    pub const fn cite(self) -> &'static str {
        match self {
            Self::IntelSdmVol4 => "Intel SDM Vol. 4, 335592-093, Table 2-1",
            Self::IntelFutureFeatures => "Intel 319433-062, Table 1-1",
            Self::IntelPerfmon => "Intel perfmon mapfile.csv at 409ce6f",
            Self::IntelMicrocode => "Intel microcode-20261110 release notes",
            Self::AmdApmVol3 => "AMD APM Vol. 3, 24594 rev. 3.39, E.3.2",
            Self::AmdRevisionGuide58251 => "AMD 58251 rev. 1.30",
            Self::AmdRevisionGuide58730 => "AMD 58730 rev. 1.30",
            Self::AmdRevisionGuide57095 => "AMD 57095 rev. 1.05",
            Self::AmdRevisionGuide57926 => "AMD 57926 rev. 1.05",
            Self::AmdRevisionGuide56323 => "AMD 56323 rev. 1.03",
            Self::AmdRevisionGuide55449 => "AMD 55449 rev. 1.21",
            Self::AmdPpr2026 => "AMD 58264 and 58655 (2026-09-30)",
        }
    }

    /// The maker that published it.
    #[must_use]
    pub const fn maker(self) -> Maker {
        match self {
            Self::IntelSdmVol4
            | Self::IntelFutureFeatures
            | Self::IntelPerfmon
            | Self::IntelMicrocode => Maker::Intel,
            _ => Maker::Amd,
        }
    }
}

/// Whether a generation is in the declared scope (ADR-0009 A1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// In the declared scope, as far as CPUID can tell (A2).
    Declared,
    /// Outside it: a bench machine. It may be used for testing, and no support or
    /// qualification claim rests on it (§76, rules 30 and 40).
    Bench,
}

/// Why a row's answer is what it is: A2's rule, in the words the line prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Intel, family 6, a model A2 lists.
    IntelListedModel,
    /// Intel, family 18 or higher.
    IntelFamily18,
    /// Intel, family 6, a model A2 does not list.
    IntelOlderModel,
    /// Intel, below family 18 and not family 6.
    IntelOlderFamily,
    /// AMD, family 1AH or higher.
    AmdZen5,
    /// AMD, below family 1AH.
    AmdBeforeZen5,
    /// Neither Intel nor AMD.
    OtherMaker,
}

impl Reason {
    /// The scope this reason gives.
    #[must_use]
    pub const fn scope(self) -> Scope {
        match self {
            Self::IntelListedModel | Self::IntelFamily18 | Self::AmdZen5 => Scope::Declared,
            Self::IntelOlderModel
            | Self::IntelOlderFamily
            | Self::AmdBeforeZen5
            | Self::OtherMaker => Scope::Bench,
        }
    }

    const fn text(self) -> &'static str {
        match self {
            Self::IntelListedModel => "a 2026 Intel generation, listed by its family-6 model",
            Self::IntelFamily18 => "Intel family 18 (12h) or higher is a 2026 generation",
            Self::IntelOlderModel => "an Intel family-6 model that is not a listed 2026 generation",
            Self::IntelOlderFamily => "an Intel family below 18 (12h) that is not family 6",
            Self::AmdZen5 => {
                "AMD family 1Ah or higher is Zen 5 or newer; CPUID gives no year, so a Zen 5 generation from 2024 reads the same"
            }
            Self::AmdBeforeZen5 => "AMD family below 1Ah, where Zen 5 begins",
            Self::OtherMaker => "neither Intel nor AMD",
        }
    }
}

/// One row of the table: which signatures it covers, what it calls them, and what follows.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    /// The maker it covers.
    pub maker: Maker,
    /// The families it covers, inclusive.
    pub families: (u32, u32),
    /// The models it covers; empty for every model.
    pub models: &'static [u32],
    /// The core generation, in the vendor's own name where it gives one.
    pub generation: &'static str,
    /// A2's answer, and why.
    pub reason: Reason,
    /// ADR-0017 Z9's form for this maker and family.
    pub form: Form,
    /// The vendor documents it rests on. A row resting on A2's rule alone, a catch-all, names
    /// none.
    pub sources: &'static [Document],
}

impl Row {
    /// Whether the row covers a signature.
    #[must_use]
    pub fn covers(&self, signature: &Signature) -> bool {
        self.maker == signature.maker
            && (self.families.0..=self.families.1).contains(&signature.family)
            && (self.models.is_empty() || self.models.contains(&signature.model))
    }
}

/// The largest family leaf 1 can report: 0FH plus an 8-bit extended family.
pub const MAX_FAMILY: u32 = 0xF + 0xFF;

use Document::{
    AmdApmVol3, AmdPpr2026, AmdRevisionGuide55449, AmdRevisionGuide56323, AmdRevisionGuide57095,
    AmdRevisionGuide57926, AmdRevisionGuide58251, AmdRevisionGuide58730, IntelFutureFeatures,
    IntelMicrocode, IntelPerfmon, IntelSdmVol4,
};

const fn intel6(
    models: &'static [u32],
    generation: &'static str,
    reason: Reason,
    sources: &'static [Document],
) -> Row {
    Row {
        maker: Maker::Intel,
        families: (6, 6),
        models,
        generation,
        reason,
        form: Form::Mask,
        sources,
    }
}

const fn intel_family(
    family: u32,
    models: &'static [u32],
    generation: &'static str,
    sources: &'static [Document],
) -> Row {
    Row {
        maker: Maker::Intel,
        families: (family, family),
        models,
        generation,
        reason: Reason::IntelFamily18,
        form: Form::Fence,
        sources,
    }
}

/// The table, read top to bottom: the first row that covers a signature answers for it. The
/// last row of each maker covers every signature that maker can report, so every signature
/// has an answer (a host test checks it).
pub static TABLE: [Row; 32] = [
    // Intel, family 6: the 2026 generations A2 lists.
    intel6(
        &[0xCC],
        "Panther Lake",
        Reason::IntelListedModel,
        &[
            IntelSdmVol4,
            IntelFutureFeatures,
            IntelMicrocode,
            IntelPerfmon,
        ],
    ),
    intel6(
        &[0xD5],
        "Wildcat Lake",
        Reason::IntelListedModel,
        &[
            IntelSdmVol4,
            IntelFutureFeatures,
            IntelMicrocode,
            IntelPerfmon,
        ],
    ),
    intel6(
        &[0xDD],
        "Clearwater Forest",
        Reason::IntelListedModel,
        &[
            IntelSdmVol4,
            IntelFutureFeatures,
            IntelMicrocode,
            IntelPerfmon,
        ],
    ),
    intel6(
        &[0xE5],
        "Panther Lake's cores (Intel names no product)",
        Reason::IntelListedModel,
        &[IntelPerfmon],
    ),
    // Intel, family 6: earlier generations, named where Intel's tables name them.
    intel6(
        &[0xBD],
        "Lunar Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelFutureFeatures, IntelMicrocode],
    ),
    intel6(
        &[0xC5, 0xC6, 0xB5],
        "Arrow Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelFutureFeatures, IntelMicrocode],
    ),
    intel6(
        &[0xAD, 0xAE],
        "Granite Rapids",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelFutureFeatures, IntelMicrocode],
    ),
    intel6(
        &[0xAF],
        "Sierra Forest",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelFutureFeatures, IntelMicrocode],
    ),
    intel6(
        &[0xB6],
        "Grand Ridge",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelFutureFeatures],
    ),
    intel6(
        &[0xAA],
        "Meteor Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0xCF],
        "Emerald Rapids",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelMicrocode],
    ),
    intel6(
        &[0x8F],
        "Sapphire Rapids",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelMicrocode],
    ),
    intel6(
        &[0xBA, 0xB7, 0xBF],
        "Raptor Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4, IntelMicrocode],
    ),
    intel6(
        &[0x97, 0x9A],
        "Alder Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0xA7],
        "Rocket Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0x8C, 0x8D],
        "Tiger Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0x7E],
        "Ice Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0x6A, 0x6C],
        "Ice Lake (Xeon)",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0x55],
        "Skylake, Cascade Lake or Cooper Lake (Xeon)",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0xA5, 0xA6],
        "Comet Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0x8E, 0x9E],
        "Kaby Lake or Coffee Lake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[0x4E, 0x5E],
        "Skylake",
        Reason::IntelOlderModel,
        &[IntelSdmVol4],
    ),
    intel6(
        &[],
        "an earlier Intel family-6 generation",
        Reason::IntelOlderModel,
        &[],
    ),
    // Intel, family 12H (18) and above: a 2026 generation, whatever the model.
    intel_family(
        0x12,
        &[0x01],
        "Nova Lake (desktop)",
        &[IntelFutureFeatures, IntelPerfmon],
    ),
    intel_family(
        0x12,
        &[0x03],
        "Nova Lake (mobile)",
        &[IntelFutureFeatures, IntelPerfmon],
    ),
    intel_family(0x13, &[0x01], "Diamond Rapids", &[IntelFutureFeatures]),
    Row {
        maker: Maker::Intel,
        families: (0x12, MAX_FAMILY),
        models: &[],
        generation: "a generation Intel has not yet listed",
        reason: Reason::IntelFamily18,
        form: Form::Fence,
        sources: &[],
    },
    Row {
        maker: Maker::Intel,
        families: (0xF, 0xF),
        models: &[],
        generation: "the Pentium 4 generation",
        reason: Reason::IntelOlderFamily,
        form: Form::Fence,
        sources: &[IntelSdmVol4],
    },
    Row {
        maker: Maker::Intel,
        families: (0, 0x11),
        models: &[],
        generation: "no x86-64 generation Intel lists",
        reason: Reason::IntelOlderFamily,
        form: Form::Fence,
        sources: &[],
    },
    // AMD: the family alone decides (A2), and Z9's mask holds on all AMD processors.
    Row {
        maker: Maker::Amd,
        families: (0x1A, MAX_FAMILY),
        models: &[],
        generation: "Zen 5 or newer",
        reason: Reason::AmdZen5,
        form: Form::Mask,
        sources: &[
            AmdApmVol3,
            AmdRevisionGuide58251,
            AmdRevisionGuide58730,
            AmdPpr2026,
        ],
    },
    Row {
        maker: Maker::Amd,
        families: (0, 0x19),
        models: &[],
        generation: "older than Zen 5",
        reason: Reason::AmdBeforeZen5,
        form: Form::Mask,
        sources: &[
            AmdApmVol3,
            AmdRevisionGuide57095,
            AmdRevisionGuide57926,
            AmdRevisionGuide56323,
            AmdRevisionGuide55449,
        ],
    },
    // Anyone else.
    Row {
        maker: Maker::Other,
        families: (0, MAX_FAMILY),
        models: &[],
        generation: "not a generation this table names",
        reason: Reason::OtherMaker,
        form: Form::Fence,
        sources: &[],
    },
];

/// An erratum a vendor documents and the kernel acts on, keyed by maker, family, model and a
/// range of steppings (ADR-0009 A2).
#[derive(Clone, Copy, Debug)]
pub struct Erratum {
    /// The maker.
    pub maker: Maker,
    /// The family.
    pub family: u32,
    /// The model.
    pub model: u32,
    /// The steppings it affects, inclusive.
    pub steppings: (u32, u32),
    /// The vendor's own identifier for it.
    pub id: &'static str,
    /// The vendor's document and revision that describes it.
    pub document: &'static str,
    /// Where the kernel works around it.
    pub workaround: &'static str,
}

impl Erratum {
    /// Whether it affects a signature.
    #[must_use]
    pub fn affects(&self, signature: &Signature) -> bool {
        self.maker == signature.maker
            && self.family == signature.family
            && self.model == signature.model
            && (self.steppings.0..=self.steppings.1).contains(&signature.stepping)
    }
}

/// The errata the kernel works around. **None**: nothing the kernel runs today works around a
/// documented erratum. An entry goes here only with its vendor's document, identifier and
/// revision, and only once code acts on it (ledger §5).
pub static ERRATA: [Erratum; 0] = [];

/// What the table says about one processor.
#[derive(Clone, Copy, Debug)]
pub struct Identity {
    /// What CPUID reported.
    pub signature: Signature,
    /// The row that answers for it.
    pub row: &'static Row,
    /// How many of [`ERRATA`] affect it.
    pub errata: usize,
}

/// The first row of `table` that covers `signature`, if any.
#[must_use]
pub fn row_in<'a>(table: &'a [Row], signature: &Signature) -> Option<&'a Row> {
    table.iter().find(|row| row.covers(signature))
}

/// The errata of `list` that affect `signature`.
pub fn errata_in<'a>(
    list: &'a [Erratum],
    signature: &'a Signature,
) -> impl Iterator<Item = &'a Erratum> + 'a {
    list.iter()
        .filter(move |erratum| erratum.affects(signature))
}

/// The row for a signature that no row covers: outside the scope, with the fence. The table's
/// own catch-alls make it unreachable (a host test shows every signature has a row); it is
/// here so that a table that ever lost one still fails closed.
static UNPLACED: Row = Row {
    maker: Maker::Other,
    families: (0, 0),
    models: &[],
    generation: "not placed by the table",
    reason: Reason::OtherMaker,
    form: Form::Fence,
    sources: &[],
};

/// What the table says about a signature.
#[must_use]
pub fn identify(signature: Signature) -> Identity {
    let row = row_in(&TABLE, &signature).unwrap_or(&UNPLACED);
    Identity {
        signature,
        row,
        errata: errata_in(&ERRATA, &signature).count(),
    }
}

impl Identity {
    /// Whether the processor is in the declared scope.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.row.reason.scope()
    }

    /// The form of [`crate::nospec`] for this processor (ADR-0017 Z9).
    #[must_use]
    pub const fn form(&self) -> Form {
        self.row.form
    }
}

/// The boot's line, after `origin-kernel: CPU identity: `. One line, whatever the processor.
impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = &self.signature;
        match s.maker {
            Maker::Intel | Maker::Amd => {
                f.write_str(if s.maker == Maker::Intel {
                    "GenuineIntel"
                } else {
                    "AuthenticAMD"
                })?;
                write!(
                    f,
                    " family {:02X}h ({}) model {:02X}h stepping {}",
                    s.family, s.family, s.model, s.stepping
                )?;
            }
            Maker::Other => {
                f.write_str("vendor \"")?;
                for &letter in &s.vendor {
                    let shown = if letter.is_ascii_graphic() || letter == b' ' {
                        char::from(letter)
                    } else {
                        '?'
                    };
                    write!(f, "{shown}")?;
                }
                write!(
                    f,
                    "\" family {:02X}h model {:02X}h stepping {} by Intel's rule",
                    s.family, s.model, s.stepping
                )?;
            }
        }
        write!(f, " (leaf 1 EAX {:08X}h): {}; ", s.eax, self.row.generation)?;
        f.write_str(match self.scope() {
            Scope::Declared => "in the declared scope: ",
            Scope::Bench => "outside the declared scope, a bench machine: ",
        })?;
        write!(f, "{} (ADR-0009 A2)", self.row.reason.text())?;
        if s.hosted {
            f.write_str("; reported under a hypervisor, which can report any identity")?;
        }
        match self.errata {
            0 => f.write_str("; no erratum listed for it"),
            1 => f.write_str("; 1 erratum listed for it"),
            n => write!(f, "; {n} errata listed for it"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::ToString;

    const INTEL: [u32; 4] = [0x23, 0x756E_6547, 0x6C65_746E, 0x4965_6E69];
    /// AMD APM Vol. 3, Table E-1: EBX 68747541H, ECX 444D4163H, EDX 69746E65H.
    const AMD: [u32; 4] = [0x10, 0x6874_7541, 0x444D_4163, 0x6974_6E65];
    const OTHER: [u32; 4] = [0xD, 0x554D_4551, 0x2055_5043, 0x7472_6956];

    /// Leaf 1 EAX for a family, model and stepping, written as the vendors' manuals lay the
    /// fields out (Intel SDM Vol. 1 §21.1.2; AMD APM Vol. 3 E.3.2), independently of
    /// [`Signature::new`]: base family 0FH and an extended family from 10H; the model's high
    /// nibble in the extended model field.
    fn eax(family: u32, model: u32, stepping: u32) -> u32 {
        let (base, extended) = if family >= 0xF {
            (0xF, family - 0xF)
        } else {
            (family, 0)
        };
        extended << 20 | (model >> 4) << 16 | base << 8 | (model & 0xF) << 4 | stepping
    }

    fn intel(family: u32, model: u32, stepping: u32) -> Signature {
        Signature::new(INTEL, eax(family, model, stepping), 0)
    }

    fn amd(family: u32, model: u32, stepping: u32) -> Signature {
        Signature::new(AMD, eax(family, model, stepping), 0)
    }

    /// ADR-0017 Z9's rule as `nospec::choose` had it before the table (decision item 132):
    /// the mask on AMD and on Intel family 6; the fence on anything else.
    fn z9(maker: Maker, family: u32) -> Form {
        match (maker, family) {
            (Maker::Amd, _) | (Maker::Intel, 6) => Form::Mask,
            _ => Form::Fence,
        }
    }

    /// A2's rule, written from its text and nothing else: AMD family 1AH or higher; Intel
    /// family 18 or higher, or family 6 with model CCH, E5H, D5H or DDH.
    fn a2(signature: &Signature) -> Scope {
        let declared = match signature.maker {
            Maker::Amd => signature.family >= 0x1A,
            Maker::Intel => {
                signature.family >= 18
                    || signature.family == 6 && [0xCC, 0xE5, 0xD5, 0xDD].contains(&signature.model)
            }
            Maker::Other => false,
        };
        if declared {
            Scope::Declared
        } else {
            Scope::Bench
        }
    }

    #[test]
    fn the_maker_comes_from_leaf_0_s_twelve_letters() {
        assert_eq!(Maker::of(INTEL), Maker::Intel);
        assert_eq!(Maker::of(AMD), Maker::Amd);
        assert_eq!(Maker::of(OTHER), Maker::Other);
        assert_eq!(Maker::of([0; 4]), Maker::Other);
        assert_eq!(&vendor_letters(AMD), b"AuthenticAMD");
        // One letter off in any of the three registers is not the maker.
        for register in 1..4 {
            let mut near = AMD;
            near[register] ^= 0x20;
            assert_eq!(Maker::of(near), Maker::Other, "register {register}");
        }
        // The registers in the wrong order are not the maker either.
        assert_eq!(Maker::of([0, AMD[1], AMD[3], AMD[2]]), Maker::Other);
    }

    #[test]
    fn the_signatures_the_vendors_publish_decode_as_their_documents_say() {
        // AMD's revision guides: leaf 1 EAX, and the family, model and stepping it names.
        for (raw, family, model, stepping) in [
            (0x00B0_0F21, 0x1A, 0x02, 1), // 58251: Turin-Zen5-C1
            (0x00B1_0F10, 0x1A, 0x11, 0), // 58730: Turin-Zen5c-B0
            (0x00A1_0F11, 0x19, 0x11, 1), // 57095: Genoa-B1
            (0x00A1_0F12, 0x19, 0x11, 2), // 57095: Genoa-B2
            (0x00AA_0F02, 0x19, 0xA0, 2), // 57926: Zen4c-A2
            (0x0083_0F10, 0x17, 0x31, 0), // 56323: Rome-B0
            (0x0080_0F12, 0x17, 0x01, 2), // 55449: ZP-B2
            (0x00A6_0F12, 0x19, 0x61, 2), // Rental A's EPYC 4244P, as Linux reported it there
        ] {
            let s = Signature::new(AMD, raw, 0);
            assert_eq!(
                (s.family, s.model, s.stepping),
                (family, model, stepping),
                "{raw:#x}"
            );
            assert_eq!(raw, eax(family, model, stepping));
        }
        // Intel's microcode release notes: family-model-stepping.
        for (family, model, stepping) in [
            (6, 0xCC, 2), // PTL-H
            (6, 0xCC, 3), // PTL-404
            (6, 0xD5, 1), // WCL
            (6, 0xDD, 1), // CWF
            (6, 0xAD, 1), // GNR
            (6, 0x8F, 8), // SPR
        ] {
            let s = intel(family, model, stepping);
            assert_eq!((s.family, s.model, s.stepping), (family, model, stepping));
        }
        // Panther Lake H, by hand: extended model C, base family 6, base model C, stepping 2.
        assert_eq!(Signature::new(INTEL, 0x000C_06C2, 0).model, 0xCC);
        // Nova Lake: base family 0FH, extended family 3, so family 12H.
        let nova = Signature::new(INTEL, 0x0030_0F10, 0);
        assert_eq!((nova.family, nova.model), (0x12, 0x01));
    }

    #[test]
    fn each_vendor_s_own_rule_decides_whether_the_extended_model_counts() {
        // Base family 6 with an extended model: Intel counts it, AMD does not (APM E.3.2).
        let raw = 0x0005_0675;
        assert_eq!(Signature::new(INTEL, raw, 0).model, 0x57);
        assert_eq!(Signature::new(AMD, raw, 0).model, 0x07);
        // Another base family below 0FH: neither counts it, nor the extended family.
        let raw = 0x0FF5_0575;
        let (i, a) = (Signature::new(INTEL, raw, 0), Signature::new(AMD, raw, 0));
        assert_eq!((i.family, i.model), (5, 0x07));
        assert_eq!((a.family, a.model), (5, 0x07));
        // Base family 0FH: both count the extended model and add the extended family.
        let raw = eax(0x1A, 0x44, 0);
        assert_eq!(Signature::new(AMD, raw, 0).model, 0x44);
        assert_eq!(Signature::new(INTEL, raw, 0).model, 0x44);
        // The hypervisor bit is ECX bit 31, and nothing else.
        assert!(Signature::new(AMD, raw, 1 << 31).hosted);
        assert!(!Signature::new(AMD, raw, !(1 << 31)).hosted);
    }

    #[test]
    fn every_signature_has_one_answer_and_it_is_a2_s() {
        for leaf0 in [INTEL, AMD, OTHER] {
            for family in 0..=MAX_FAMILY {
                for model in 0..=0xFF {
                    let s = Signature::new(leaf0, eax(family, model, 0), 0);
                    if s.family != family || s.model != model {
                        // A model this maker's rule cannot report for this family.
                        continue;
                    }
                    let row = row_in(&TABLE, &s).expect("every signature has a row");
                    assert_eq!(
                        row.reason.scope(),
                        a2(&s),
                        "{:?} family {family:#x} model {model:#x}: {}",
                        s.maker,
                        row.generation
                    );
                    assert_eq!(row.form, z9(s.maker, family), "{:?} {family:#x}", s.maker);
                }
            }
        }
    }

    #[test]
    fn the_boundaries_fall_where_a2_puts_them() {
        let declared = |s: Signature| identify(s).scope() == Scope::Declared;
        // AMD: 19H is not, 1AH and every family above it is.
        assert!(!declared(amd(0x19, 0x61, 2)));
        assert!(!declared(amd(0x19, 0xFF, 0)));
        assert!(declared(amd(0x1A, 0x00, 0)));
        assert!(declared(amd(0x1B, 0x00, 0)));
        assert!(declared(amd(MAX_FAMILY, 0xFF, 0xF)));
        assert!(!declared(amd(0x0F, 0x00, 0)));
        // Intel family 6: each listed model, and not its neighbours.
        for model in [0xCC, 0xD5, 0xDD, 0xE5] {
            assert!(declared(intel(6, model, 0)), "{model:#x}");
            assert!(!declared(intel(6, model - 1, 0)), "{:#x}", model - 1);
            assert!(!declared(intel(6, model + 1, 0)), "{:#x}", model + 1);
        }
        // Intel families 15 to 17 are not; 18 and above are, whatever the model.
        for family in [0xF, 0x10, 0x11] {
            assert!(!declared(intel(family, 0x01, 0)), "{family:#x}");
        }
        for family in [0x12, 0x13, 0x14, MAX_FAMILY] {
            for model in [0x00, 0x01, 0x03, 0x7F, 0xFF] {
                assert!(declared(intel(family, model, 0)), "{family:#x} {model:#x}");
            }
        }
        // Another maker that reports a family and model in the scope is not.
        assert!(!declared(Signature::new(OTHER, eax(0x1A, 0x02, 0), 0)));
        assert!(!declared(Signature::new(OTHER, eax(6, 0xCC, 0), 0)));
    }

    #[test]
    fn the_form_follows_the_vendors_statements() {
        // ADR-0017 Z9, moved here from `nospec::choose` with the same answers. AMD's guidance
        // covers every family, in and out of scope.
        for family in [0x0F, 0x17, 0x19, 0x1A, 0x1B] {
            assert_eq!(
                identify(amd(family, 0, 0)).form(),
                Form::Mask,
                "AMD {family:#x}"
            );
        }
        assert_eq!(identify(intel(6, 0xCC, 2)).form(), Form::Mask);
        assert_eq!(identify(intel(6, 0x01, 0)).form(), Form::Mask);
        // Nova Lake is family 18 and Diamond Rapids family 19: Intel's statement of 2018 and
        // 2021 does not reach them. Nor does anything say it for an unknown maker.
        for family in [5, 15, 18, 19, 20] {
            assert_eq!(
                identify(intel(family, 1, 0)).form(),
                Form::Fence,
                "Intel {family}"
            );
        }
        for family in [6, 18, 0x1A] {
            let s = Signature::new(OTHER, eax(family, 1, 0), 0);
            assert_eq!(identify(s).form(), Form::Fence, "unknown, family {family}");
        }
    }

    #[test]
    fn the_rows_are_well_formed() {
        for (at, row) in TABLE.iter().enumerate() {
            assert!(row.families.0 <= row.families.1, "row {at}");
            assert!(row.families.1 <= MAX_FAMILY, "row {at}");
            assert!(!row.generation.is_empty(), "row {at}");
            // A row is named after the vendor's documents, and only that vendor's.
            for document in row.sources {
                assert_eq!(document.maker(), row.maker, "row {at}: {}", document.cite());
            }
            // A row that names one model, or a few, names them from a document: only the
            // catch-alls rest on A2's rule alone.
            if !row.models.is_empty() {
                assert!(!row.sources.is_empty(), "row {at}: {}", row.generation);
            }
            // A row of the declared scope that is not the rule's own catch-all rests on a
            // document, and no row puts in the scope what A2 leaves out (the exhaustive
            // test checks that for every signature).
            if row.reason.scope() == Scope::Declared && !row.models.is_empty() {
                assert!(!row.sources.is_empty(), "row {at}");
            }
            // No model is listed twice for one family, here or in an earlier row.
            for (other_at, other) in TABLE[..at].iter().enumerate() {
                if other.maker == row.maker
                    && other.families.0 <= row.families.1
                    && row.families.0 <= other.families.1
                {
                    for model in row.models {
                        assert!(
                            !other.models.contains(model),
                            "model {model:#x} in rows {other_at} and {at}"
                        );
                    }
                }
            }
        }
        // Every row is reached by some signature: no row hides behind an earlier one.
        for (at, row) in TABLE.iter().enumerate() {
            let leaf0 = match row.maker {
                Maker::Intel => INTEL,
                Maker::Amd => AMD,
                Maker::Other => OTHER,
            };
            let reached = (row.families.0..=row.families.1).any(|family| {
                (0..=0xFF).any(|model| {
                    let s = Signature::new(leaf0, eax(family, model, 0), 0);
                    core::ptr::eq(row_in(&TABLE, &s).unwrap(), row)
                })
            });
            assert!(reached, "row {at} ({}) is never reached", row.generation);
        }
    }

    #[test]
    fn an_empty_table_fails_closed() {
        assert!(row_in(&[], &amd(0x1A, 0, 0)).is_none());
        assert_eq!(UNPLACED.reason.scope(), Scope::Bench);
        assert_eq!(UNPLACED.form, Form::Fence);
    }

    #[test]
    fn errata_match_on_maker_family_model_and_stepping() {
        // The shipped list is empty: nothing the kernel runs works around an erratum.
        assert!(ERRATA.is_empty());
        // The lookup, over a list that exists only here.
        let list = [
            Erratum {
                maker: Maker::Intel,
                family: 6,
                model: 0xCC,
                steppings: (2, 3),
                id: "T1",
                document: "a test",
                workaround: "none",
            },
            Erratum {
                maker: Maker::Amd,
                family: 0x1A,
                model: 0x02,
                steppings: (1, 1),
                id: "T2",
                document: "a test",
                workaround: "none",
            },
        ];
        let ids = |s: Signature| {
            errata_in(&list, &s)
                .map(|e| e.id)
                .collect::<alloc::vec::Vec<_>>()
        };
        assert_eq!(ids(intel(6, 0xCC, 2)), ["T1"]);
        assert_eq!(ids(intel(6, 0xCC, 3)), ["T1"]);
        assert!(ids(intel(6, 0xCC, 1)).is_empty());
        assert!(ids(intel(6, 0xCC, 4)).is_empty());
        assert!(ids(intel(6, 0xCD, 2)).is_empty());
        assert!(ids(amd(6, 0xCC, 2)).is_empty(), "the maker counts");
        assert_eq!(ids(amd(0x1A, 0x02, 1)), ["T2"]);
        assert!(ids(amd(0x1B, 0x02, 1)).is_empty());
        assert!(ids(Signature::new(OTHER, eax(0x1A, 0x02, 1), 0)).is_empty());
    }

    #[test]
    fn the_line_says_what_was_reported_what_it_is_and_why() {
        let ptl = identify(Signature::new(INTEL, 0x000C_06C2, 1 << 31));
        assert_eq!(
            ptl.to_string(),
            "GenuineIntel family 06h (6) model CCh stepping 2 (leaf 1 EAX 000C06C2h): Panther Lake; \
             in the declared scope: a 2026 Intel generation, listed by its family-6 model (ADR-0009 A2); \
             reported under a hypervisor, which can report any identity; no erratum listed for it"
        );
        let rental = identify(Signature::new(AMD, 0x00A6_0F12, 0));
        assert_eq!(
            rental.to_string(),
            "AuthenticAMD family 19h (25) model 61h stepping 2 (leaf 1 EAX 00A60F12h): older than Zen 5; \
             outside the declared scope, a bench machine: AMD family below 1Ah, where Zen 5 begins \
             (ADR-0009 A2); no erratum listed for it"
        );
        let turin = identify(Signature::new(AMD, 0x00B0_0F21, 0));
        assert_eq!(
            turin.to_string(),
            "AuthenticAMD family 1Ah (26) model 02h stepping 1 (leaf 1 EAX 00B00F21h): Zen 5 or newer; \
             in the declared scope: AMD family 1Ah or higher is Zen 5 or newer; CPUID gives no year, \
             so a Zen 5 generation from 2024 reads the same (ADR-0009 A2); no erratum listed for it"
        );
        let nova = identify(Signature::new(INTEL, 0x0030_0F10, 0));
        assert!(nova.to_string().starts_with(
            "GenuineIntel family 12h (18) model 01h stepping 0 (leaf 1 EAX 00300F10h): Nova Lake (desktop); \
             in the declared scope: Intel family 18 (12h) or higher"
        ));
        let other = identify(Signature::new(OTHER, 0x0000_0663, 0));
        assert_eq!(
            other.to_string(),
            "vendor \"QEMUVirtCPU \" family 06h model 06h stepping 3 by Intel's rule (leaf 1 EAX 00000663h): \
             not a generation this table names; outside the declared scope, a bench machine: neither \
             Intel nor AMD (ADR-0009 A2); no erratum listed for it"
        );
        // Letters that would break the line are shown as '?'.
        let odd = identify(Signature::new([0, 0x0A00_0041, 0, 0], 0, 0));
        assert!(
            odd.to_string().starts_with("vendor \"A???????????\""),
            "{odd}"
        );
        // One line, always.
        for s in [ptl, rental, turin, nova, other, odd] {
            assert!(!format!("{s}").contains('\n'));
        }
    }
}
