//! `\_OSI`, `\_OS` and `\_REV` (ACPI 6.6 §5.7.2 to §5.7.4; ADR-0019 T5).
//!
//! **Pending owner item 309.** The policy the contract's §6 proposes, built as proposed until the
//! owner answers: `\_OSI` answers `Ones` for the Windows strings Microsoft lists (its page "How to
//! identify the Windows version in ACPI by using _OSI", last updated 2024-09-11), because Windows
//! answers True for every earlier version and firmware in Origin's scope is validated against it;
//! `Zero` for everything else, `Linux` and the other vendors' strings included, and for each
//! ACPI-defined feature-group string of Table 5.248 until the task that builds that feature adds
//! it. `\_REV` is 2, the only value §5.7.4 allows an interpreter with 64-bit integers.
//!
//! **`\_OS` is provisional**: "Origin OS", the truthful name, until the ACPICA reading session
//! (TASKS 64) answers G10 with evidence of what deployed firmware expects.

use crate::Interpreter;
use crate::error::{ErrorKind, Result, err};
use crate::host::Host;
use crate::object::{Object, Val};

/// The strings `\_OSI` answers `Ones` to. Pending owner item 309.
pub const OSI_STRINGS: &[&[u8]] = &[
    b"Windows 2000",
    b"Windows 2001",
    b"Windows 2001 SP1",
    b"Windows 2001.1",
    b"Windows 2001 SP2",
    b"Windows 2001.1 SP1",
    b"Windows 2006",
    b"Windows 2006 SP1",
    b"Windows 2006.1",
    b"Windows 2009",
    b"Windows 2012",
    b"Windows 2013",
    b"Windows 2015",
    b"Windows 2016",
    b"Windows 2017",
    b"Windows 2017.2",
    b"Windows 2018",
    b"Windows 2018.2",
    b"Windows 2019",
    b"Windows 2020",
    b"Windows 2021",
    b"Windows 2022",
];

/// `\_OS`. Provisional until G10 is answered.
pub const OS_NAME: &[u8] = b"Origin OS";

/// `\_REV`. Pending owner item 309.
pub const REV: u64 = 2;

impl<H: Host> Interpreter<H> {
    /// `\_OSI (arg)`.
    pub(crate) fn osi(&mut self, arg: Val) -> Result<Val> {
        let Val::Obj(r) = arg else {
            return err(ErrorKind::Type);
        };
        let Object::String(s) = self.heap.get(r)? else {
            return err(ErrorKind::Type);
        };
        let yes = OSI_STRINGS.contains(&s.as_slice());
        Ok(Val::Int(if yes { self.width.mask() } else { 0 }))
    }
}
