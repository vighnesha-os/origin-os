//! What each processor reports about speculation, and what the kernel does with it
//! (ADR-0017 Z1, Z2, Z3, Z5 and the form of Z9).
//!
//! This file is the decision and the record, in safe code a host can test:
//!
//! - [`Reported`] is what a processor was asked: CPUID leaf 07H and its subleaves,
//!   Fn8000_0008, Fn8000_0021, and `IA32_ARCH_CAPABILITIES` where it exists.
//! - [`Plan::decide`] turns that into what the kernel sets on that processor. Every setting
//!   follows a bit the processor reports, never its name, with one exception the ADR makes:
//!   the form of [`crate::nospec`], which no bit reports. That comes from the CPU
//!   identification table ([`crate::cpuident`], ADR-0009 A2), which ADR-0017 makes its home.
//! - Each processor's plan, and whether its registers read back as planned, is kept per
//!   processor ([`record`], [`recorded`]): the cores of one machine may differ.
//! - [`Ledger`] counts changes of process on a processor, for the IBPB of Z3.
//!
//! `cpu.rs` reads the registers and writes them. The kernel never refuses to boot on what it
//! finds here (Z1): it prints it.
//!
//! The bit positions are the vendors', as read on 2026-11-10 into
//! `docs/research/2026-11-10-speculation-processor-hardening.md` (§3.1 and §3.2): Intel SDM
//! 335592-093 and Intel's guidance pages, AMD's APM Volume 3 (24594, Rev. 3.39) and Volume 2
//! (24593, Rev. 3.46), and AMD's white paper on managing speculation (revision 5.09.23).

#![forbid(unsafe_code)]

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::cpuident::{self, Maker, Signature};
use crate::limits::MAX_CPUS;
use crate::nospec::Form;

/// `IA32_SPEC_CTRL` bit 0: with enhanced IBRS, set once and left.
pub const IBRS: u64 = 1 << 0;
/// `IA32_SPEC_CTRL` bit 1: no sibling thread steers this one's indirect branches.
pub const STIBP: u64 = 1 << 1;
/// `IA32_SPEC_CTRL` bit 6: no alternate predictor for returns in ring 0.
pub const RRSBA_DIS_S: u64 = 1 << 6;
/// `IA32_SPEC_CTRL` bit 10: branch history from ring 3 does not pick ring 0's targets.
pub const BHI_DIS_S: u64 = 1 << 10;

/// `IA32_ARCH_CAPABILITIES` bit 1: enhanced IBRS.
const IBRS_ALL: u64 = 1 << 1;
/// `IA32_ARCH_CAPABILITIES` bit 19: returns may use the alternate predictor.
const RRSBA: u64 = 1 << 19;
/// `IA32_ARCH_CAPABILITIES` bit 20: not affected by branch history injection.
const BHI_NO: u64 = 1 << 20;

const fn bit(value: u32, n: u32) -> bool {
    value & (1 << n) != 0
}

/// What a processor reports. A leaf it does not have is all zeros.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reported {
    /// CPUID leaf 0: the highest basic leaf, then the maker's name in EBX, ECX and EDX.
    pub leaf0: [u32; 4],
    /// CPUID leaf 1, EAX (family, model, stepping) and ECX (bit 31: a hypervisor).
    pub leaf1: [u32; 2],
    /// CPUID leaf 07H, subleaves 0, 1 and 2.
    pub leaf7: [[u32; 4]; 3],
    /// CPUID Fn8000_0008, EBX.
    pub ext8_ebx: u32,
    /// CPUID Fn8000_0021.
    pub ext21: [u32; 4],
    /// `IA32_ARCH_CAPABILITIES`, where CPUID.07H.00H:EDX[29] says it exists; else 0.
    pub arch_capabilities: u64,
}

impl Reported {
    /// The maker, from the name in leaf 0.
    #[must_use]
    pub fn maker(&self) -> Maker {
        Maker::of(self.leaf0)
    }

    /// The family: the base family, plus the extended family where the base is 0FH.
    #[must_use]
    pub const fn family(&self) -> u32 {
        cpuident::family_of(self.leaf1[0])
    }

    /// The maker, family, model and stepping, as the CPU identification table reads them.
    #[must_use]
    pub fn signature(&self) -> Signature {
        Signature::new(self.leaf0, self.leaf1[0], self.leaf1[1])
    }

    /// CPUID.01H:ECX[31]: a hypervisor runs this processor.
    const fn hosted(&self) -> bool {
        bit(self.leaf1[1], 31)
    }
}

/// What the kernel knows about LFENCE on a processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lfence {
    /// It is a barrier: Intel says always; AMD reports it (Fn8000_0021_EAX[2]), or the
    /// family is one AMD says always serializes.
    Barrier,
    /// AMD, not reported: the kernel sets MSR C001_1029 bit 1, which AMD says every family
    /// from 10H on has but 11H.
    KernelSets,
    /// Not reported, and not the kernel's to set: a hypervisor owns that register, or no
    /// document read says anything about this maker.
    Unknown,
}

/// What ADR-0017 needs and a processor does not offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lack {
    /// Neither enhanced IBRS nor automatic IBRS.
    Isolation,
    /// Affected by branch history injection, and no `BHI_DIS_S`.
    BhiControl,
    /// Returns use the alternate predictor, and no `RRSBA_DIS_S`.
    RrsbaControl,
    /// No IBPB.
    Ibpb,
    /// An IBPB that leaves return predictions in place.
    IbpbReturns,
    /// Nothing that keeps a sibling thread from steering this one's branches.
    SiblingIsolation,
    /// An LFENCE that is not known to be a barrier.
    Lfence,
}

impl Lack {
    /// Every lack, in the order the boot line names them.
    pub const ALL: [Self; 7] = [
        Self::Isolation,
        Self::BhiControl,
        Self::RrsbaControl,
        Self::Ibpb,
        Self::IbpbReturns,
        Self::SiblingIsolation,
        Self::Lfence,
    ];

    const fn mask(self) -> u64 {
        1 << (self as u64)
    }

    /// The name the boot line gives it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Isolation => "enhanced or automatic IBRS",
            Self::BhiControl => "BHI_DIS_S",
            Self::RrsbaControl => "RRSBA_DIS_S",
            Self::Ibpb => "IBPB",
            Self::IbpbReturns => "an IBPB that clears return predictions",
            Self::SiblingIsolation => "branch isolation between sibling threads",
            Self::Lfence => "an LFENCE known to be a barrier",
        }
    }
}

/// What the kernel sets on one processor, and what that processor lacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The value of `IA32_SPEC_CTRL`, where the processor has the register. The kernel
    /// writes it once and never clears a bit of it (Z2, Z5).
    pub spec_ctrl: Option<u64>,
    /// Whether to set EFER.AIBRSE: AMD's automatic IBRS (Z2).
    pub automatic_ibrs: bool,
    /// Whether the processor takes the IBPB command (Z3).
    pub ibpb: bool,
    /// LFENCE on this processor (Z9).
    pub lfence: Lfence,
    /// The form of `nospec` for this processor (Z9), from the CPU identification table.
    pub form: Form,
    lacks: u64,
}

impl Plan {
    /// The plan for a processor that reports `reported`.
    #[must_use]
    pub fn decide(reported: &Reported) -> Self {
        let maker = reported.maker();
        let family = reported.family();
        let seven = reported.leaf7[0][3];
        let controls = reported.leaf7[2][3];
        let amd = reported.ext8_ebx;
        let capabilities = reported.arch_capabilities;

        // The register exists if any bit of it is enumerated, by either vendor's leaf.
        let register = bit(seven, 26)
            || bit(seven, 27)
            || bit(seven, 31)
            || bit(amd, 14)
            || bit(amd, 15)
            || bit(amd, 24);
        // Enhanced IBRS: the IBRS bit exists, and the capability register says it may stay
        // set. That register is read only where CPUID.07H.00H:EDX[29] reports it.
        let enhanced = bit(seven, 26) && bit(seven, 29) && capabilities & IBRS_ALL != 0;
        let automatic = bit(reported.ext21[0], 8);
        // AMD: STIBP exists, and Fn8000_0008_EBX[17] recommends leaving it on.
        let stibp = bit(amd, 15) && bit(amd, 17);
        let affected_by_history = enhanced && capabilities & BHI_NO == 0;
        let alternate_returns = enhanced && capabilities & RRSBA != 0;

        let mut value = 0;
        if enhanced {
            value |= IBRS;
        }
        if affected_by_history && bit(controls, 4) {
            value |= BHI_DIS_S;
        }
        if alternate_returns && bit(controls, 2) {
            value |= RRSBA_DIS_S;
        }
        if stibp {
            value |= STIBP;
        }

        let ibpb = bit(seven, 26) || bit(amd, 12);
        // Intel defines IBPB as clearing return predictions too. AMD reports it: IBPB_RET.
        let clears_returns = match maker {
            Maker::Intel => bit(seven, 26),
            Maker::Amd | Maker::Other => bit(amd, 30),
        };

        let lfence = match maker {
            Maker::Intel => Lfence::Barrier,
            Maker::Amd if bit(reported.ext21[0], 2) => Lfence::Barrier,
            Maker::Amd if reported.hosted() => Lfence::Unknown,
            // AMD's mitigation G-2: families 0FH and 11H always serialize and have no such
            // register; every family from 10H on has it.
            Maker::Amd if family == 0x0F || family == 0x11 => Lfence::Barrier,
            Maker::Amd if family >= 0x10 => Lfence::KernelSets,
            Maker::Amd | Maker::Other => Lfence::Unknown,
        };

        let mut lacks = 0;
        if !enhanced && !automatic {
            lacks |= Lack::Isolation.mask();
        }
        if affected_by_history && !bit(controls, 4) {
            lacks |= Lack::BhiControl.mask();
        }
        if alternate_returns && !bit(controls, 2) {
            lacks |= Lack::RrsbaControl.mask();
        }
        if !ibpb {
            lacks |= Lack::Ibpb.mask();
        } else if !clears_returns {
            lacks |= Lack::IbpbReturns.mask();
        }
        // Intel: enhanced IBRS also keeps a sibling thread out. AMD: STIBP does.
        if !enhanced && !stibp {
            lacks |= Lack::SiblingIsolation.mask();
        }
        if matches!(lfence, Lfence::Unknown) {
            lacks |= Lack::Lfence.mask();
        }

        Self {
            spec_ctrl: if register { Some(value) } else { None },
            automatic_ibrs: automatic,
            ibpb,
            lfence,
            form: cpuident::identify(reported.signature()).form(),
            lacks,
        }
    }

    /// Whether the processor lacks `lack`.
    #[must_use]
    pub const fn lacks(&self, lack: Lack) -> bool {
        self.lacks & lack.mask() != 0
    }

    /// Whether the processor offers everything ADR-0017 needs.
    #[must_use]
    pub const fn complete(&self) -> bool {
        self.lacks == 0
    }

    /// The plan as one word, for an atomic. Never 0.
    const fn to_bits(self) -> u64 {
        let (present, value) = match self.spec_ctrl {
            Some(value) => (1, value & 0xFFFF),
            None => (0, 0),
        };
        let lfence = match self.lfence {
            Lfence::Barrier => 0,
            Lfence::KernelSets => 1,
            Lfence::Unknown => 2,
        };
        let form = match self.form {
            Form::Mask => 0,
            Form::Fence => 1,
        };
        RECORDED
            | value
            | present << 16
            | (self.automatic_ibrs as u64) << 17
            | (self.ibpb as u64) << 18
            | lfence << 19
            | form << 21
            | (self.lacks & 0xFF) << 24
    }

    const fn from_bits(bits: u64) -> Self {
        Self {
            spec_ctrl: if bits & (1 << 16) != 0 {
                Some(bits & 0xFFFF)
            } else {
                None
            },
            automatic_ibrs: bits & (1 << 17) != 0,
            ibpb: bits & (1 << 18) != 0,
            lfence: match (bits >> 19) & 3 {
                0 => Lfence::Barrier,
                1 => Lfence::KernelSets,
                _ => Lfence::Unknown,
            },
            form: if bits & (1 << 21) == 0 {
                Form::Mask
            } else {
                Form::Fence
            },
            lacks: (bits >> 24) & 0xFF,
        }
    }
}

/// The plan as the boot line prints it: what is on, then what the processor lacks.
impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.spec_ctrl.unwrap_or(0);
        let on = [
            (value & IBRS != 0, "enhanced IBRS"),
            (value & BHI_DIS_S != 0, "BHI_DIS_S"),
            (value & RRSBA_DIS_S != 0, "RRSBA_DIS_S"),
            (self.automatic_ibrs, "automatic IBRS"),
            (value & STIBP != 0, "STIBP"),
            (self.ibpb, "IBPB at each change of process"),
            (
                matches!(self.lfence, Lfence::KernelSets),
                "LFENCE made a barrier",
            ),
        ];
        f.write_str("on: ")?;
        let mut any = false;
        for (_, name) in on.iter().filter(|(set, _)| *set) {
            if any {
                f.write_str(", ")?;
            }
            f.write_str(name)?;
            any = true;
        }
        if !any {
            f.write_str("nothing")?;
        }
        f.write_str(match self.form {
            Form::Mask => "; lookups: mask",
            Form::Fence => "; lookups: fence",
        })?;
        f.write_str("; not offered: ")?;
        let mut any = false;
        for lack in Lack::ALL.iter().filter(|lack| self.lacks(**lack)) {
            if any {
                f.write_str(", ")?;
            }
            f.write_str(lack.name())?;
            any = true;
        }
        if !any {
            f.write_str("nothing")?;
        }
        Ok(())
    }
}

/// Set in every recorded word, so a recorded plan is never 0.
const RECORDED: u64 = 1 << 62;
/// Set where the processor's registers read back as the plan says.
const HELD: u64 = 1 << 61;

/// Each processor's plan, as [`Plan::to_bits`], with [`HELD`]. 0: nothing recorded.
static RECORDS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Records what processor `cpu` decided and whether its registers read back that way. Each
/// processor does it on itself, once, as it starts.
pub fn record(cpu: usize, plan: Plan, held: bool) {
    if let Some(slot) = RECORDS.get(cpu) {
        let bits = plan.to_bits() | if held { HELD } else { 0 };
        slot.store(bits, Ordering::Relaxed);
    }
}

/// What processor `cpu` recorded: its plan, and whether its registers read back as planned.
#[must_use]
pub fn recorded(cpu: usize) -> Option<(Plan, bool)> {
    let bits = RECORDS.get(cpu)?.load(Ordering::Relaxed);
    (bits & RECORDED != 0).then(|| (Plan::from_bits(bits), bits & HELD != 0))
}

/// What one processor has counted of the processes it ran in ring 3 (Z3). The first two
/// wrap together after 2^32.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    /// Times it took up a process other than the last one it ran.
    pub changes: u32,
    /// IBPBs it was told to issue.
    pub barriers: u32,
    /// Times it took the same process up again.
    pub resumes: u64,
}

/// One processor's memory of the last process it ran in ring 3, and its counts.
#[derive(Debug)]
pub struct Ledger {
    /// The last process's ID plus one. 0: none yet.
    last: AtomicU64,
    /// Changes in the low half and barriers in the high half, in one word: another processor
    /// that reads it sees a change and its barrier together or not at all.
    changes_and_barriers: AtomicU64,
    resumes: AtomicU64,
}

impl Ledger {
    /// A processor that has run no process.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: AtomicU64::new(0),
            changes_and_barriers: AtomicU64::new(0),
            resumes: AtomicU64::new(0),
        }
    }

    /// Notes that the processor is about to run `process` in ring 3, and says whether it
    /// must issue an IBPB first: when the process is not the last one it ran, and the
    /// processor has the command (`offered`). Kernel threads in between change nothing, and
    /// nor does another thread of the same process.
    ///
    /// Only the processor itself calls this, with interrupts off: it is the one writer, so
    /// a load and a store are enough.
    pub fn enters(&self, process: u64, offered: bool) -> bool {
        let tagged = process.wrapping_add(1);
        if self.last.swap(tagged, Ordering::Relaxed) == tagged {
            self.resumes.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let Count {
            changes, barriers, ..
        } = self.count();
        let changes = changes.wrapping_add(1);
        let barriers = barriers.wrapping_add(u32::from(offered));
        self.changes_and_barriers.store(
            u64::from(changes) | u64::from(barriers) << 32,
            Ordering::Relaxed,
        );
        offered
    }

    /// The counts so far.
    #[must_use]
    pub fn count(&self) -> Count {
        let both = self.changes_and_barriers.load(Ordering::Relaxed);
        Count {
            changes: both as u32,
            barriers: (both >> 32) as u32,
            resumes: self.resumes.load(Ordering::Relaxed),
        }
    }
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

static LEDGERS: [Ledger; MAX_CPUS] = [const { Ledger::new() }; MAX_CPUS];

/// [`Ledger::enters`] for processor `cpu`, with what that processor recorded about IBPB.
/// False for a processor that recorded nothing: it has not said the command exists.
pub fn enters(cpu: usize, process: u64) -> bool {
    let offered = recorded(cpu).is_some_and(|(plan, _)| plan.ibpb);
    LEDGERS
        .get(cpu)
        .is_some_and(|ledger| ledger.enters(process, offered))
}

/// What processor `cpu` has counted.
#[must_use]
pub fn count(cpu: usize) -> Count {
    LEDGERS.get(cpu).map(Ledger::count).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    const INTEL: [u32; 4] = [0x23, 0x756E_6547, 0x6C65_746E, 0x4965_6E69];
    const AMD: [u32; 4] = [0x10, 0x6874_7541, 0x444D_4163, 0x6974_6E65];

    /// Leaf 1 EAX for a family and model.
    fn signature(family: u32, model: u32) -> u32 {
        let (base, extended) = if family >= 0xF {
            (0xF, family - 0xF)
        } else {
            (family, 0)
        };
        extended << 20 | (model >> 4) << 16 | base << 8 | (model & 0xF) << 4
    }

    fn bits(positions: &[u32]) -> u32 {
        positions.iter().fold(0, |word, n| word | 1 << n)
    }

    /// AMD Family 1AH Model 51H, Revision B0, as its Processor Programming Reference gives
    /// the registers (publication 58264, Rev 0.45, 2026-09-23).
    fn amd_model_51h() -> Reported {
        Reported {
            leaf0: AMD,
            leaf1: [signature(0x1A, 0x51), 0],
            leaf7: [[1, 0, bits(&[7]), 0], [bits(&[17, 18]), 0, 0, 0], [0; 4]],
            // IBPB, IBRS, STIBP, StibpAlwaysOn, IbrsPreferred, IbrsSameMode, SSBD, PSFD,
            // BTC_NO, IBPB_RET.
            ext8_ebx: bits(&[12, 14, 15, 17, 18, 19, 24, 28, 29, 30]),
            ext21: [
                // LFenceAlwaysSerializing, AutomaticIBRS, ERAPS, SBPB, IBPB_BRTYPE,
                // SRSO_USER_KERNEL_NO.
                bits(&[2, 8, 24, 27, 28, 30]),
                0x08 << 16,
                // TSA_SQ_NO, TSA_L1_NO, BtbCtxIsolation.
                bits(&[1, 2, 8]),
                0,
            ],
            arch_capabilities: 0,
        }
    }

    /// AMD Family 1AH Model 02H, Revision C1: Zen 5 in a server (publication 57238, Rev
    /// 0.49, 2026-03-06). SRSO_MSR_FIX is 1, and no Fn8000_0021_ECX is documented.
    fn amd_model_02h() -> Reported {
        let mut reported = amd_model_51h();
        reported.leaf1[0] = signature(0x1A, 0x02);
        reported.leaf7[1] = [0; 4];
        reported.ext21[0] |= 1 << 31;
        reported.ext21[2] = 0;
        reported
    }

    /// An Intel processor with enhanced IBRS, built from the SDM's bit positions: no
    /// processor's own values were read. `capabilities` is `IA32_ARCH_CAPABILITIES`.
    fn intel(family: u32, capabilities: u64, subleaf2: u32) -> Reported {
        Reported {
            leaf0: INTEL,
            leaf1: [signature(family, 0xC5), 0],
            // IBRS and IBPB, STIBP, ARCH_CAPABILITIES, SSBD.
            leaf7: [
                [2, 0, 0, bits(&[26, 27, 29, 31])],
                [0; 4],
                [0, 0, 0, subleaf2],
            ],
            ext8_ebx: 0,
            ext21: [0; 4],
            arch_capabilities: capabilities,
        }
    }

    #[test]
    fn the_maker_and_family_are_read_from_leaves_0_and_1() {
        assert_eq!(amd_model_51h().maker(), Maker::Amd);
        assert_eq!(amd_model_51h().family(), 0x1A);
        assert_eq!(amd_model_51h().leaf1[0], 0x00B5_0F10);
        assert_eq!(intel(6, 0, 0).maker(), Maker::Intel);
        assert_eq!(intel(6, 0, 0).family(), 6);
        // Nova Lake and Diamond Rapids: base family 0FH and an extended family.
        assert_eq!(intel(18, 0, 0).family(), 18);
        assert_eq!(intel(19, 0, 0).family(), 19);
        assert_eq!(Reported::default().maker(), Maker::Other);
        // One letter off is not the maker.
        let mut near = amd_model_51h();
        near.leaf0[2] ^= 1;
        assert_eq!(near.maker(), Maker::Other);
    }

    #[test]
    fn amd_s_published_values_give_automatic_ibrs_stibp_and_the_mask() {
        for reported in [amd_model_51h(), amd_model_02h()] {
            let plan = Plan::decide(&reported);
            assert_eq!(plan.spec_ctrl, Some(STIBP));
            assert!(plan.automatic_ibrs && plan.ibpb);
            assert_eq!(plan.lfence, Lfence::Barrier);
            assert_eq!(plan.form, Form::Mask);
            assert!(plan.complete(), "{plan}");
            assert_eq!(
                plan.to_string(),
                "on: automatic IBRS, STIBP, IBPB at each change of process; lookups: mask; \
                 not offered: nothing"
            );
        }
    }

    #[test]
    fn enhanced_ibrs_is_set_with_the_two_controls_only_where_they_apply() {
        // Affected by branch history, returns on the alternate predictor, both controls.
        let plan = Plan::decide(&intel(6, IBRS_ALL | RRSBA, bits(&[2, 4])));
        assert_eq!(plan.spec_ctrl, Some(IBRS | BHI_DIS_S | RRSBA_DIS_S));
        assert!(!plan.automatic_ibrs && plan.ibpb && plan.complete());
        assert_eq!((plan.lfence, plan.form), (Lfence::Barrier, Form::Mask));
        // Not affected by either: the controls exist and stay clear.
        let plan = Plan::decide(&intel(6, IBRS_ALL | BHI_NO, bits(&[2, 4])));
        assert_eq!(plan.spec_ctrl, Some(IBRS));
        assert!(plan.complete());
        // Affected, and the processor has neither control: it says so.
        let plan = Plan::decide(&intel(6, IBRS_ALL | RRSBA, 0));
        assert_eq!(plan.spec_ctrl, Some(IBRS));
        assert!(plan.lacks(Lack::BhiControl) && plan.lacks(Lack::RrsbaControl));
        assert_eq!(
            plan.to_string(),
            "on: enhanced IBRS, IBPB at each change of process; lookups: mask; \
             not offered: BHI_DIS_S, RRSBA_DIS_S"
        );
    }

    #[test]
    fn a_newer_intel_family_gets_the_fence() {
        let plan = Plan::decide(&intel(18, IBRS_ALL | BHI_NO, 0));
        assert_eq!(plan.form, Form::Fence);
        assert_eq!(plan.spec_ctrl, Some(IBRS));
        assert!(plan.complete());
    }

    #[test]
    fn the_capability_register_counts_only_where_cpuid_reports_it() {
        // IBRS without enhanced IBRS: the register exists and the kernel sets nothing in
        // it. Writing IBRS on every entry is not built (Z2).
        let mut old = intel(6, 0, 0);
        let plan = Plan::decide(&old);
        assert_eq!(plan.spec_ctrl, Some(0));
        assert!(plan.lacks(Lack::Isolation) && plan.lacks(Lack::SiblingIsolation));
        assert!(plan.ibpb && !plan.lacks(Lack::Ibpb) && !plan.lacks(Lack::IbpbReturns));
        // A value in the field with CPUID.07H.00H:EDX[29] clear is not a capability.
        old.leaf7[0][3] &= !(1 << 29);
        old.arch_capabilities = IBRS_ALL;
        assert_eq!(Plan::decide(&old).spec_ctrl, Some(0));
    }

    #[test]
    fn a_processor_that_reports_nothing_gets_no_write_and_says_what_it_lacks() {
        let mut bare = Reported {
            leaf0: AMD,
            leaf1: [signature(0x0F, 0x6B), 0],
            ..Reported::default()
        };
        let plan = Plan::decide(&bare);
        assert_eq!(plan.spec_ctrl, None);
        assert!(!plan.automatic_ibrs && !plan.ibpb);
        // AMD's family 0FH always serializes LFENCE and has no register for it.
        assert_eq!(plan.lfence, Lfence::Barrier);
        assert_eq!(
            plan.to_string(),
            "on: nothing; lookups: mask; not offered: enhanced or automatic IBRS, IBPB, \
             branch isolation between sibling threads"
        );
        // A maker no document read speaks for: the fence, and an LFENCE nobody vouches for.
        bare.leaf0 = [0; 4];
        let plan = Plan::decide(&bare);
        assert_eq!((plan.form, plan.lfence), (Form::Fence, Lfence::Unknown));
        assert!(plan.lacks(Lack::Lfence));
    }

    #[test]
    fn amd_s_lfence_is_reported_set_by_the_kernel_or_left_to_a_host() {
        // Family 17H that does not report it: the kernel sets the register's bit.
        let mut zen = amd_model_51h();
        zen.leaf1[0] = signature(0x17, 0x31);
        zen.ext21[0] &= !(1 << 2);
        let plan = Plan::decide(&zen);
        assert_eq!(plan.lfence, Lfence::KernelSets);
        assert!(plan.complete());
        assert!(plan.to_string().contains("LFENCE made a barrier"));
        // The same under a hypervisor: the register is the host's.
        zen.leaf1[1] = 1 << 31;
        let plan = Plan::decide(&zen);
        assert_eq!(plan.lfence, Lfence::Unknown);
        assert!(plan.lacks(Lack::Lfence));
        // Family 11H has no such register and always serializes.
        zen.leaf1 = [signature(0x11, 0x03), 0];
        assert_eq!(Plan::decide(&zen).lfence, Lfence::Barrier);
    }

    #[test]
    fn an_ibpb_that_leaves_return_predictions_is_named() {
        let mut zen = amd_model_51h();
        zen.ext8_ebx &= !(1 << 30);
        let plan = Plan::decide(&zen);
        assert!(plan.ibpb && plan.lacks(Lack::IbpbReturns) && !plan.lacks(Lack::Ibpb));
        // STIBP that AMD does not recommend leaving on is not set.
        zen.ext8_ebx &= !(1 << 17);
        let plan = Plan::decide(&zen);
        assert_eq!(plan.spec_ctrl, Some(0));
        assert!(plan.lacks(Lack::SiblingIsolation));
    }

    #[test]
    fn settings_follow_the_bits_whatever_the_name_says() {
        // An emulator can give one maker's name to the other's processor. Each setting still
        // follows its own bit; only the form and LFENCE follow the name, as nothing else can.
        let mut mixed = amd_model_51h();
        mixed.leaf0 = INTEL;
        let plan = Plan::decide(&mixed);
        assert!(plan.automatic_ibrs);
        assert_eq!(plan.spec_ctrl, Some(STIBP));
        assert_eq!(plan.form, Form::Fence, "Intel, family 26");
    }

    #[test]
    fn a_plan_survives_its_record() {
        let plans = [
            Plan::decide(&amd_model_51h()),
            Plan::decide(&intel(6, IBRS_ALL | RRSBA, bits(&[2, 4]))),
            Plan::decide(&intel(18, IBRS_ALL | BHI_NO, 0)),
            Plan::decide(&intel(6, 0, 0)),
            Plan::decide(&Reported::default()),
        ];
        for plan in plans {
            assert_ne!(plan.to_bits(), 0);
            assert_eq!(Plan::from_bits(plan.to_bits()), plan, "{plan}");
        }
        // The top of the processor table, which no other test uses.
        let cpu = MAX_CPUS - 1;
        assert_eq!(recorded(cpu), None);
        record(cpu, plans[0], true);
        assert_eq!(recorded(cpu), Some((plans[0], true)));
        record(cpu, plans[3], false);
        assert_eq!(recorded(cpu), Some((plans[3], false)));
        assert_eq!(recorded(MAX_CPUS), None);
        record(MAX_CPUS, plans[0], true);
    }

    #[test]
    fn one_barrier_for_each_change_of_process_and_none_without() {
        let ledger = Ledger::new();
        // The first process on a processor is a change: what ran before is unknown.
        assert!(ledger.enters(7, true));
        // The same process again, as another of its threads or after kernel threads.
        assert!(!ledger.enters(7, true));
        assert!(!ledger.enters(7, true));
        assert!(ledger.enters(8, true));
        assert!(ledger.enters(7, true));
        // Process 0 is a process like any other.
        assert!(ledger.enters(0, true));
        assert!(!ledger.enters(0, true));
        assert_eq!(
            ledger.count(),
            Count {
                changes: 4,
                barriers: 4,
                resumes: 3,
            }
        );
    }

    #[test]
    fn a_processor_without_ibpb_counts_changes_and_issues_nothing() {
        let ledger = Ledger::new();
        assert!(!ledger.enters(1, false));
        assert!(!ledger.enters(2, false));
        assert!(!ledger.enters(2, false));
        assert_eq!(
            ledger.count(),
            Count {
                changes: 2,
                barriers: 0,
                resumes: 1,
            }
        );
        // A processor that recorded nothing has not said it has the command.
        let cpu = MAX_CPUS - 2;
        assert!(!enters(cpu, 1));
        assert_eq!(count(cpu).barriers, 0);
        record(cpu, Plan::decide(&amd_model_51h()), true);
        assert!(enters(cpu, 2));
        assert!(!enters(cpu, 2));
        assert_eq!(count(cpu).barriers, 1);
        assert_eq!(count(MAX_CPUS), Count::default());
    }
}
