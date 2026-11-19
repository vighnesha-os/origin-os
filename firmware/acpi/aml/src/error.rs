//! What can go wrong, and where.
//!
//! Every function of the crate returns these; nothing panics. An error carries the table and the
//! offset in its term list where the interpreter was when it happened, once the engine knows them.

use crate::host::HostError;

/// An error, with where it happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    /// What went wrong.
    pub kind: ErrorKind,
    /// Where: the table and the byte offset in its term list (the 36-byte header not counted).
    /// `None` for an error outside any table, such as a path given to the API.
    pub at: Option<At>,
}

/// A place in a loaded table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct At {
    /// The table, numbered from 0 in load order.
    pub table: u32,
    /// The offset in its term list.
    pub offset: u32,
}

/// The limits of ADR-0019 A3 (the contract's §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    /// `While` iterations in one evaluation.
    Loops,
    /// Time in one evaluation, by the host's clock.
    Time,
    /// Method calls nested in one evaluation.
    Depth,
    /// Bytes the interpreter holds.
    Memory,
    /// Opcodes executed in one evaluation.
    Steps,
    /// Package nesting in a value handed across the API (64 levels).
    Nesting,
}

/// What went wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The term list ends inside a construct.
    Truncated,
    /// A package length that is malformed or reaches past its container (ACPI 6.6 §5.4.1).
    PackageLength,
    /// An opcode Table 20.2 does not define, or reserves (`0x5B` and `second` for the two-byte
    /// ones).
    Reserved { first: u8, second: Option<u8> },
    /// A malformed name or path.
    Name,
    /// A construct where the grammar does not allow it: a statement where a value is needed, an
    /// `Else` without its `If`, a data object that is not one.
    Grammar,
    /// A name that resolves to nothing.
    NotFound,
    /// A name created twice in one scope (§5.3). Strict until the reading session answers (G3).
    Duplicate,
    /// A `Scope` whose target cannot hold names, or a field whose region or register is not one.
    WrongObject,
    /// An operand whose type the operator cannot take, or that cannot be converted.
    Type,
    /// An uninitialized object used as an operand (Table 19.5).
    Uninitialized,
    /// A method's value used where it returned none (G6: ACPICA's default too, TASKS 64's ledger, `docs/research/2026-11-14-aml-gaps-acpica.md` §3).
    NoValue,
    /// `Divide` or `Mod` by zero.
    DivideByZero,
    /// An index, field or length outside its object.
    Range,
    /// `Break` or `Continue` outside a `While`.
    NotInLoop,
    /// A handle to an object, node or frame that no longer exists.
    Stale,
    /// A limit was reached.
    Limit(Limit),
    /// The host refused or failed a request that is not a region access (the Global Lock, a
    /// table).
    Host(HostError),
    /// The host refused or failed an access to an operation region: its space, the address, and
    /// why.
    Region {
        space: u8,
        address: u64,
        cause: HostError,
    },
    /// A mutex acquired below the current sync level, or released at another (ACPI 6.6
    /// §19.6.89; ACPI 4.0's release rule). G2: ACPICA's default too, with no lenient mode (TASKS
    /// 64's ledger §3).
    SyncLevel,
    /// `Release` of a mutex this evaluation does not hold (§19.6.117).
    NotOwned,
    /// A wait that only another evaluation could end (`Wait` with no timeout on an event with no
    /// signal pending): the interpreter runs one evaluation at a time, so it would never end.
    WouldBlock,
    /// `Fatal` (§19.6.47): the firmware's report, also given to the host.
    Fatal { kind: u8, code: u32, arg: u64 },
    /// A path given to the API that is not one.
    Path,
    /// The method belongs to a table whose load failed and was rolled back.
    TableFailed,
    /// Finding a PCI_Config region's function needed an access to that same region, through
    /// `_ADR` or the like (the review of B2, B2-1): refused, never a recursion.
    AddressCycle,
    /// Internal: a PCI_Config region's function must be found before the access that wanted it.
    /// The engine puts the step back and finds it; never returned by the API.
    #[doc(hidden)]
    Pending,
}

impl Error {
    /// An error with no place yet.
    pub(crate) const fn new(kind: ErrorKind) -> Self {
        Self { kind, at: None }
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self::new(kind)
    }
}

/// The crate's result.
pub type Result<T> = core::result::Result<T, Error>;

/// Shorthand for an error of `kind` with no place.
pub(crate) const fn err<T>(kind: ErrorKind) -> Result<T> {
    Err(Error::new(kind))
}
