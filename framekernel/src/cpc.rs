//! `_CPC`, ACPI's Continuous Performance Control (ACPI 6.6 §8.4.6.1; ADR-0020 S9, source 2;
//! TASKS item 106): the adapter from the AML interpreter's values to `capacity::CpcElement`, and
//! what the AML proof found of it.
//!
//! **Not evaluated in the kernel but on QEMU's tables.** ADR-0019 A2 (decision item 302) runs the
//! firmware's AML in the user-mode ACPI service, and A3 gives the kernel only its boot proof on
//! QEMU's own tables, with every region refused. So `_CPC` is evaluated only inside that proof
//! (`aml_proof`), which counts what it finds; capacity's source 2 is empty at boot on every machine
//! (the coordinating session's ruling Q1 (a) of 2026-11-21). On AMD machines without the HFI, and
//! with CPPC not enabled by the firmware, capacity is therefore equal at M3.
//!
//! **Every register is refused here.** FFH has no published encoding for `_CPC`: Intel's
//! *Processor Vendor-Specific ACPI* (302223-007) defines FFH for `_PCT`, `_CST` and `_PTC` only, and
//! no AMD document read defines one. PCC is ADR-0019's area H, and SystemMemory and SystemIO are the
//! ACPI service's to be granted (A2). So only an Integer Highest Performance gives a value
//! ([`highest`]); `_OSC`'s bit 12 is never set, since no `_OSC` is evaluated (area D).

use alloc::vec::Vec;

use origin_aml::Value;
use origin_frame::capacity::{self, CpcElement, CpcError};
use origin_frame::once::SetOnce;

/// One element of a `_CPC` package as `capacity::cpc_highest` reads it: an Integer, a Buffer (a
/// register's resource template), or anything else.
pub fn element(value: &Value) -> CpcElement<'_> {
    match value {
        Value::Integer(v) => CpcElement::Integer(*v),
        Value::Buffer(b) => CpcElement::Buffer(b),
        _ => CpcElement::Other,
    }
}

/// `_CPC`'s Highest Performance from `value`, the object's result, with every register refused: an
/// Integer Highest, or why there is none ([`CpcError::Unread`] for a register, which is never read
/// here). A value that is not a package has no entries.
pub fn highest(value: &Value) -> Result<u32, CpcError> {
    let Value::Package(items) = value else {
        return Err(CpcError::NumEntries);
    };
    let elements: Vec<CpcElement<'_>> = items.iter().map(element).collect();
    capacity::cpc_highest(&elements, |_| None)
}

/// What one processor's `_CPC` gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// An Integer Highest Performance.
    Integer(u32),
    /// Highest Performance in a register: not read in the kernel.
    Register,
    /// The evaluation was refused: a method that reached a region, which the boot's host refuses.
    Refused,
    /// A package that is not a `_CPC` this kernel reads.
    Malformed(CpcError),
}

/// What `result`, one processor's evaluation of its `_CPC`, gave: `refused` says whether its
/// failure was one of the refusals the boot's host makes.
pub fn outcome(result: Result<Option<&Value>, bool>) -> Outcome {
    match result {
        Err(true) => Outcome::Refused,
        Err(false) | Ok(None) => Outcome::Malformed(CpcError::NumEntries),
        Ok(Some(value)) => match highest(value) {
            Ok(h) => Outcome::Integer(h),
            Err(CpcError::Unread) => Outcome::Register,
            Err(e) => Outcome::Malformed(e),
        },
    }
}

/// What the AML proof found of `_CPC` on QEMU's processors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Processors reached.
    pub processors: u32,
    /// Of them, those with a `_CPC`.
    pub with_cpc: u32,
    /// Of those, those whose Highest is an Integer.
    pub integer: u32,
    /// Those whose Highest is a register, not read.
    pub register: u32,
    /// Those whose evaluation was refused.
    pub refused: u32,
    /// Those whose package this kernel does not read.
    pub malformed: u32,
}

impl Found {
    /// Counts one processor's outcome, or none where it has no `_CPC`.
    pub fn count(&mut self, outcome: Option<Outcome>) {
        self.processors = self.processors.saturating_add(1);
        let Some(outcome) = outcome else {
            return;
        };
        self.with_cpc = self.with_cpc.saturating_add(1);
        let slot = match outcome {
            Outcome::Integer(_) => &mut self.integer,
            Outcome::Register => &mut self.register,
            Outcome::Refused => &mut self.refused,
            Outcome::Malformed(_) => &mut self.malformed,
        };
        *slot = slot.saturating_add(1);
    }
}

/// What the boot's AML proof found, once it has run on QEMU's tables; empty on any other firmware,
/// where it runs no AML.
pub static FOUND: SetOnce<Found> = SetOnce::new();

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    fn package(items: Vec<Value>) -> Value {
        Value::Package(items)
    }

    /// A `_CPC` package of `count` entries, revision `revision`, Highest `highest`, every other
    /// entry an Integer 0.
    fn cpc(count: u64, revision: u64, highest: Value) -> Value {
        let mut items = vec![Value::Integer(count), Value::Integer(revision), highest];
        items.resize(usize::try_from(count).unwrap(), Value::Integer(0));
        package(items)
    }

    /// A Generic Register Descriptor in `space`, and the end tag.
    fn register(space: u8, width: u8, address: u64) -> Value {
        let mut b = vec![0x82, 12, 0, space, width, 0, 4];
        b.extend_from_slice(&address.to_le_bytes());
        b.extend_from_slice(&[0x79, 0]);
        Value::Buffer(b)
    }

    /// Revisions 2 to 4, Highest an Integer, a register in each space the kernel refuses, and the
    /// NULL register.
    #[test]
    fn only_an_integer_highest_is_read() {
        for revision in [2, 3, 4] {
            assert_eq!(highest(&cpc(25, revision, Value::Integer(200))), Ok(200));
            assert_eq!(
                outcome(Ok(Some(&cpc(25, revision, Value::Integer(9))))),
                Outcome::Integer(9)
            );
        }
        // FFH, PCC, SystemMemory, SystemIO: refused here, so no value.
        for space in [0x7F, 0x0A, 0x00, 0x01] {
            assert_eq!(
                highest(&cpc(25, 4, register(space, 8, 0x1000))),
                Err(CpcError::Unread),
                "space {space:#x}"
            );
            assert_eq!(
                outcome(Ok(Some(&cpc(25, 4, register(space, 8, 0x1000))))),
                Outcome::Register
            );
        }
        // The NULL register marks Highest unsupported, which it may not be.
        assert_eq!(
            highest(&cpc(25, 4, register(0, 0, 0))),
            Err(CpcError::NullRegister)
        );
    }

    /// What is not a `_CPC` this kernel reads.
    #[test]
    fn malformed_packages_and_refusals() {
        // NumEntries not the package's length.
        let mut short = cpc(25, 4, Value::Integer(1));
        if let Value::Package(items) = &mut short {
            items.pop();
        }
        assert_eq!(highest(&short), Err(CpcError::NumEntries));
        // A revision this kernel does not know.
        assert_eq!(
            highest(&cpc(25, 5, Value::Integer(1))),
            Err(CpcError::Revision)
        );
        // Highest a string, a package, or 0.
        assert_eq!(
            highest(&cpc(25, 4, Value::String(b"x".to_vec()))),
            Err(CpcError::Highest)
        );
        assert_eq!(
            highest(&cpc(25, 4, package(vec![]))),
            Err(CpcError::Highest)
        );
        assert_eq!(
            highest(&cpc(25, 4, Value::Integer(0))),
            Err(CpcError::Value)
        );
        assert_eq!(
            highest(&cpc(25, 4, Value::Integer(1 << 32))),
            Err(CpcError::Value)
        );
        // Not a package at all.
        assert_eq!(highest(&Value::Integer(25)), Err(CpcError::NumEntries));
        assert_eq!(
            outcome(Ok(Some(&Value::Integer(25)))),
            Outcome::Malformed(CpcError::NumEntries)
        );
        assert_eq!(outcome(Err(true)), Outcome::Refused);
        assert_eq!(
            outcome(Err(false)),
            Outcome::Malformed(CpcError::NumEntries)
        );
        assert_eq!(outcome(Ok(None)), Outcome::Malformed(CpcError::NumEntries));
    }

    #[test]
    fn found_counts_each_processor_once() {
        let mut f = Found::default();
        for o in [
            None,
            Some(Outcome::Integer(5)),
            Some(Outcome::Register),
            Some(Outcome::Refused),
            Some(Outcome::Malformed(CpcError::Revision)),
            Some(Outcome::Integer(7)),
        ] {
            f.count(o);
        }
        assert_eq!(
            f,
            Found {
                processors: 6,
                with_cpc: 5,
                integer: 2,
                register: 1,
                refused: 1,
                malformed: 1,
            }
        );
    }
}
