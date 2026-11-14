//! The opcodes of ACPI 6.6 Table 20.2 (PDF pages 1144–1147) and the operands each takes
//! (§20.2.5).
//!
//! Data objects (the constants, prefixes and `String`), names, locals, arguments and `Debug` are
//! decoded by the engine directly; everything else is here, with its operand kinds. A byte value
//! Table 20.2 leaves empty or reserves decodes to nothing and is refused.

/// What an operand is, in the grammar's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `TermArg`: evaluated to a value. An argument holding a reference is dereferenced
    /// (ACPI 6.6 Table 19.9).
    TermArg,
    /// `DerefOf`'s operand: evaluated, but an argument holding a reference is not dereferenced.
    TermArgRaw,
    /// `SuperName`: a place, which must exist.
    Super,
    /// `CondRefOf`'s `SuperName`: a place that may be absent.
    SuperOpt,
    /// `Target`: a `SuperName` or the null name.
    Target,
    /// `CopyObject`'s `SimpleName`: a name, an argument or a local.
    Simple,
    /// A `NameString` to create.
    NameNew,
    /// A `NameString` naming something that exists (resolved when the operator runs).
    NameOld,
    /// A `NameString` that is only parsed (`External`).
    NameRaw,
    /// `ByteData`.
    Byte,
    /// `WordData`.
    Word,
    /// `DWordData`.
    DWord,
    /// `DefName`'s `DataRefObject`: a data object only.
    Data,
}

/// How an opcode may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Yields a value; allowed wherever a `TermArg` is.
    Expression,
    /// A statement or a named object: allowed only in a term list.
    Statement,
}

/// Every opcode the engine runs through an operand frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Alias,
    Name,
    Scope,
    Buffer,
    Package,
    VarPackage,
    Method,
    External,
    Mutex,
    Event,
    CondRefOf,
    CreateField,
    LoadTable,
    Load,
    Stall,
    Sleep,
    Acquire,
    Signal,
    Wait,
    Reset,
    Release,
    FromBcd,
    ToBcd,
    Fatal,
    Timer,
    Region,
    Field,
    Device,
    Processor,
    PowerRes,
    ThermalZone,
    IndexField,
    BankField,
    DataRegion,
    Store,
    RefOf,
    Add,
    Concat,
    Subtract,
    Increment,
    Decrement,
    Multiply,
    Divide,
    ShiftLeft,
    ShiftRight,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Not,
    FindSetLeftBit,
    FindSetRightBit,
    DerefOf,
    ConcatRes,
    Mod,
    Notify,
    SizeOf,
    Index,
    Match,
    CreateDWordField,
    CreateWordField,
    CreateByteField,
    CreateBitField,
    ObjectType,
    CreateQWordField,
    LAnd,
    LOr,
    LNot,
    LNotEqual,
    LLessEqual,
    LGreaterEqual,
    LEqual,
    LGreater,
    LLess,
    ToBuffer,
    ToDecimalString,
    ToHexString,
    ToInteger,
    ToString,
    CopyObject,
    Mid,
    Continue,
    If,
    While,
    Noop,
    Return,
    Break,
    BreakPoint,
    /// A method invocation: its operands are the method's arguments.
    Call,
}

/// An opcode's shape.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Info {
    pub(crate) op: Op,
    /// Bytes of opcode (1, or 2 for the `0x5B` and `LNot` pairs).
    pub(crate) len: u32,
    /// Whether a package length follows the opcode.
    pub(crate) pkg: bool,
    pub(crate) class: Class,
    /// The fixed operands, in order.
    pub(crate) args: &'static [Kind],
}

use Class::{Expression as E, Statement as S};
use Kind::{
    Byte, DWord, Data, NameNew, NameOld, NameRaw, Simple, Super, SuperOpt, Target, TermArg,
    TermArgRaw, Word,
};

const fn i(op: Op, pkg: bool, class: Class, args: &'static [Kind]) -> Info {
    Info {
        op,
        len: 1,
        pkg,
        class,
        args,
    }
}

const fn x(op: Op, pkg: bool, class: Class, args: &'static [Kind]) -> Info {
    Info {
        op,
        len: 2,
        pkg,
        class,
        args,
    }
}

const T2: &[Kind] = &[TermArg, TermArg, Target];
const T1: &[Kind] = &[TermArg, Target];
const L2: &[Kind] = &[TermArg, TermArg];

/// Decodes the opcode of `first` (and `second`, the byte after it, if there is one).
pub(crate) const fn decode(first: u8, second: Option<u8>) -> Option<Info> {
    Some(match first {
        0x06 => i(Op::Alias, false, S, &[NameOld, NameNew]),
        0x08 => i(Op::Name, false, S, &[NameNew, Data]),
        0x10 => i(Op::Scope, true, S, &[NameOld]),
        0x11 => i(Op::Buffer, true, E, &[TermArg]),
        0x12 => i(Op::Package, true, E, &[Byte]),
        0x13 => i(Op::VarPackage, true, E, &[TermArg]),
        0x14 => i(Op::Method, true, S, &[NameNew, Byte]),
        0x15 => i(Op::External, false, S, &[NameRaw, Byte, Byte]),
        0x5B => {
            let Some(s) = second else { return None };
            match s {
                0x01 => x(Op::Mutex, false, S, &[NameNew, Byte]),
                0x02 => x(Op::Event, false, S, &[NameNew]),
                0x12 => x(Op::CondRefOf, false, E, &[SuperOpt, Target]),
                0x13 => x(
                    Op::CreateField,
                    false,
                    S,
                    &[TermArg, TermArg, TermArg, NameNew],
                ),
                0x1F => x(
                    Op::LoadTable,
                    false,
                    E,
                    &[TermArg, TermArg, TermArg, TermArg, TermArg, TermArg],
                ),
                // §19.6.77 gives `Load` a Boolean result, though the grammar lists it nowhere
                // (E2): it is taken as an expression.
                0x20 => x(Op::Load, false, E, &[NameOld, Target]),
                0x21 => x(Op::Stall, false, S, &[TermArg]),
                0x22 => x(Op::Sleep, false, S, &[TermArg]),
                0x23 => x(Op::Acquire, false, E, &[Super, Word]),
                0x24 => x(Op::Signal, false, S, &[Super]),
                0x25 => x(Op::Wait, false, E, &[Super, TermArg]),
                0x26 => x(Op::Reset, false, S, &[Super]),
                0x27 => x(Op::Release, false, S, &[Super]),
                0x28 => x(Op::FromBcd, false, E, T1),
                0x29 => x(Op::ToBcd, false, E, T1),
                0x32 => x(Op::Fatal, false, S, &[Byte, DWord, TermArg]),
                0x33 => x(Op::Timer, false, E, &[]),
                0x80 => x(Op::Region, false, S, &[NameNew, Byte, TermArg, TermArg]),
                0x81 => x(Op::Field, true, S, &[NameOld, Byte]),
                0x82 => x(Op::Device, true, S, &[NameNew]),
                0x83 => x(Op::Processor, true, S, &[NameNew, Byte, DWord, Byte]),
                0x84 => x(Op::PowerRes, true, S, &[NameNew, Byte, Word]),
                0x85 => x(Op::ThermalZone, true, S, &[NameNew]),
                0x86 => x(Op::IndexField, true, S, &[NameOld, NameOld, Byte]),
                0x87 => x(Op::BankField, true, S, &[NameOld, NameOld, TermArg, Byte]),
                0x88 => x(
                    Op::DataRegion,
                    false,
                    S,
                    &[NameNew, TermArg, TermArg, TermArg],
                ),
                _ => return None,
            }
        }
        0x70 => i(Op::Store, false, E, &[TermArg, Super]),
        0x71 => i(Op::RefOf, false, E, &[Super]),
        0x72 => i(Op::Add, false, E, T2),
        0x73 => i(Op::Concat, false, E, T2),
        0x74 => i(Op::Subtract, false, E, T2),
        0x75 => i(Op::Increment, false, E, &[Super]),
        0x76 => i(Op::Decrement, false, E, &[Super]),
        0x77 => i(Op::Multiply, false, E, T2),
        0x78 => i(Op::Divide, false, E, &[TermArg, TermArg, Target, Target]),
        0x79 => i(Op::ShiftLeft, false, E, T2),
        0x7A => i(Op::ShiftRight, false, E, T2),
        0x7B => i(Op::And, false, E, T2),
        0x7C => i(Op::Nand, false, E, T2),
        0x7D => i(Op::Or, false, E, T2),
        0x7E => i(Op::Nor, false, E, T2),
        0x7F => i(Op::Xor, false, E, T2),
        0x80 => i(Op::Not, false, E, T1),
        0x81 => i(Op::FindSetLeftBit, false, E, T1),
        0x82 => i(Op::FindSetRightBit, false, E, T1),
        0x83 => i(Op::DerefOf, false, E, &[TermArgRaw]),
        0x84 => i(Op::ConcatRes, false, E, T2),
        0x85 => i(Op::Mod, false, E, T2),
        0x86 => i(Op::Notify, false, S, &[Super, TermArg]),
        0x87 => i(Op::SizeOf, false, E, &[Super]),
        0x88 => i(Op::Index, false, E, T2),
        0x89 => i(
            Op::Match,
            false,
            E,
            &[TermArg, Byte, TermArg, Byte, TermArg, TermArg],
        ),
        0x8A => i(Op::CreateDWordField, false, S, &[TermArg, TermArg, NameNew]),
        0x8B => i(Op::CreateWordField, false, S, &[TermArg, TermArg, NameNew]),
        0x8C => i(Op::CreateByteField, false, S, &[TermArg, TermArg, NameNew]),
        0x8D => i(Op::CreateBitField, false, S, &[TermArg, TermArg, NameNew]),
        0x8E => i(Op::ObjectType, false, E, &[Super]),
        0x8F => i(Op::CreateQWordField, false, S, &[TermArg, TermArg, NameNew]),
        0x90 => i(Op::LAnd, false, E, L2),
        0x91 => i(Op::LOr, false, E, L2),
        0x92 => match second {
            Some(0x93) => x(Op::LNotEqual, false, E, L2),
            Some(0x94) => x(Op::LLessEqual, false, E, L2),
            Some(0x95) => x(Op::LGreaterEqual, false, E, L2),
            _ => i(Op::LNot, false, E, &[TermArg]),
        },
        0x93 => i(Op::LEqual, false, E, L2),
        0x94 => i(Op::LGreater, false, E, L2),
        0x95 => i(Op::LLess, false, E, L2),
        0x96 => i(Op::ToBuffer, false, E, T1),
        0x97 => i(Op::ToDecimalString, false, E, T1),
        0x98 => i(Op::ToHexString, false, E, T1),
        0x99 => i(Op::ToInteger, false, E, T1),
        0x9C => i(Op::ToString, false, E, &[TermArg, TermArg, Target]),
        0x9D => i(Op::CopyObject, false, E, &[TermArg, Simple]),
        0x9E => i(Op::Mid, false, E, &[TermArg, TermArg, TermArg, Target]),
        0x9F => i(Op::Continue, false, S, &[]),
        0xA0 => i(Op::If, true, S, &[TermArg]),
        0xA2 => i(Op::While, true, S, &[TermArg]),
        0xA3 => i(Op::Noop, false, S, &[]),
        0xA4 => i(Op::Return, false, S, &[TermArg]),
        0xA5 => i(Op::Break, false, S, &[]),
        0xCC => i(Op::BreakPoint, false, S, &[]),
        _ => return None,
    })
}
