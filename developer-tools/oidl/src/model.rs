//! The validated model: what `sema` builds and every generator reads. By the time a value is
//! here, its names are resolved and its layouts computed, and it has passed every check in the
//! specification.

use std::rc::Rc;

pub use crate::ast::Transport;

/// `major.minor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major.
    pub major: u16,
    /// Minor.
    pub minor: u16,
}

impl core::fmt::Display for Version {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// An integer type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Int {
    /// `u8`
    U8,
    /// `u16`
    U16,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `i8`
    I8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `i64`
    I64,
}

impl Int {
    /// The type named `word`.
    pub fn from_name(word: &str) -> Option<Self> {
        Some(match word {
            "u8" => Self::U8,
            "u16" => Self::U16,
            "u32" => Self::U32,
            "u64" => Self::U64,
            "i8" => Self::I8,
            "i16" => Self::I16,
            "i32" => Self::I32,
            "i64" => Self::I64,
            _ => return None,
        })
    }

    /// Its size and alignment in bytes.
    pub const fn size(self) -> u64 {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 => 4,
            Self::U64 | Self::I64 => 8,
        }
    }

    /// Whether it is signed.
    pub const fn signed(self) -> bool {
        matches!(self, Self::I8 | Self::I16 | Self::I32 | Self::I64)
    }

    /// Whether it is exactly 64 bits wide.
    pub const fn is_64(self) -> bool {
        self.size() == 8
    }

    /// Its Rust spelling.
    pub const fn rust(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
        }
    }

    /// Its C spelling.
    pub const fn c(self) -> &'static str {
        match self {
            Self::U8 => "uint8_t",
            Self::U16 => "uint16_t",
            Self::U32 => "uint32_t",
            Self::U64 => "uint64_t",
            Self::I8 => "int8_t",
            Self::I16 => "int16_t",
            Self::I32 => "int32_t",
            Self::I64 => "int64_t",
        }
    }

    /// The smallest value.
    pub const fn min(self) -> i128 {
        match self {
            Self::U8 | Self::U16 | Self::U32 | Self::U64 => 0,
            Self::I8 => i8::MIN as i128,
            Self::I16 => i16::MIN as i128,
            Self::I32 => i32::MIN as i128,
            Self::I64 => i64::MIN as i128,
        }
    }

    /// The largest value.
    pub const fn max(self) -> i128 {
        match self {
            Self::U8 => u8::MAX as i128,
            Self::U16 => u16::MAX as i128,
            Self::U32 => u32::MAX as i128,
            Self::U64 => u64::MAX as i128,
            Self::I8 => i8::MAX as i128,
            Self::I16 => i16::MAX as i128,
            Self::I32 => i32::MAX as i128,
            Self::I64 => i64::MAX as i128,
        }
    }
}

/// One member of an `enum`, or one bit of a `bits`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// For an enum, the value; for a bits, the bit index.
    pub value: i128,
    /// When it was added.
    pub since: Version,
}

/// An `enum`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enum {
    /// Library that declares it.
    pub library: String,
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// When it was added.
    pub since: Version,
    /// Underlying type.
    pub repr: Int,
    /// Members.
    pub members: Vec<Member>,
}

/// A `bits`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bits {
    /// Library that declares it.
    pub library: String,
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// When it was added.
    pub since: Version,
    /// Underlying unsigned type.
    pub repr: Int,
    /// Bits.
    pub members: Vec<Member>,
}

impl Bits {
    /// The mask of the bits known at `minor`.
    pub fn known_mask(&self, minor: u16) -> u64 {
        self.members
            .iter()
            .filter(|m| m.since.minor <= minor)
            .fold(0, |mask, m| mask | (1u64 << m.value))
    }
}

/// A resolved `handle` type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandleType {
    /// The member of `object_type`, or `None` for `any`.
    pub object: Option<String>,
    /// That member's value (0 for `any`).
    pub object_value: u64,
    /// The members of `rights` it needs, by name.
    pub rights: Vec<String>,
    /// Their mask.
    pub rights_mask: u64,
    /// Whether it may be absent.
    pub optional: bool,
}

/// A resolved type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    /// `bool`.
    Bool,
    /// An integer.
    Int(Int),
    /// An enum.
    Enum(Rc<Enum>),
    /// A bits.
    Bits(Rc<Bits>),
    /// A struct.
    Struct(Rc<Struct>),
    /// `array<T, N>`.
    Array(Box<Type>, u32),
    /// `vector<T, N>`.
    Vector(Box<Type>, u32),
    /// `string<N>`.
    String(u32),
    /// `handle` or `optional<handle>`.
    Handle(HandleType),
}

/// One field of a struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// Type.
    pub ty: Type,
    /// When it was added.
    pub since: Version,
    /// Offset in bytes.
    pub offset: u32,
}

/// A struct, with its layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Struct {
    /// Library that declares it.
    pub library: String,
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// When it was added.
    pub since: Version,
    /// Whether it begins with a `u32 size`.
    pub sized: bool,
    /// Fields, in layout order.
    pub fields: Vec<Field>,
    /// Size in bytes, at the newest version.
    pub size: u32,
    /// Alignment.
    pub align: u32,
    /// Padding `(offset, length)`, in ascending offset.
    pub padding: Vec<(u32, u32)>,
    /// For a sized struct: the size at each version that changed it, ascending by minor.
    pub sizes: Vec<(u16, u32)>,
    /// How deeply structs nest in it: 1 if it holds none (§2).
    pub depth: u32,
    /// Whether it holds the `status` enum anywhere.
    pub has_status: bool,
}

/// A constant's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstKind {
    /// An integer.
    Int(Int),
    /// A combination of bits.
    Bits(Rc<Bits>),
}

/// A `const`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Const {
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// When it was added.
    pub since: Version,
    /// Type.
    pub kind: ConstKind,
    /// Value.
    pub value: i128,
}

/// A `capability`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    /// `a.b`.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// When it was added.
    pub since: Version,
}

impl Capability {
    /// `a.b` → `a_b`.
    pub fn ident(&self) -> String {
        self.name.replace('.', "_")
    }
}

/// A parameter or result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// Type.
    pub ty: Type,
    /// When it was added.
    pub since: Version,
    /// `@nonzero`.
    pub nonzero: bool,
    /// `@count(result.field)`.
    pub count: Option<(String, String)>,
    /// `@also_on(…)`, by name.
    pub also_on: Vec<String>,
}

/// A member of `status`, by name and value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRef {
    /// Name.
    pub name: String,
    /// Value.
    pub value: i64,
}

/// `requires x.y` (and the status of a refusal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    /// The capability.
    pub capability: Capability,
    /// The status of a refusal.
    pub denied: StatusRef,
}

/// A method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Method {
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// The number after `=`.
    pub ordinal: u32,
    /// `oneway`.
    pub oneway: bool,
    /// `-> never`.
    pub never: bool,
    /// `@reserved`.
    pub reserved: bool,
    /// When it was added.
    pub since: Version,
    /// Parameters.
    pub params: Vec<Param>,
    /// Results.
    pub returns: Vec<Param>,
    /// Declared errors, in the order written.
    pub errors: Vec<StatusRef>,
    /// `requires x.y`, or `None` for `requires none`.
    pub requires: Option<Requirement>,
}

/// An interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    /// Name.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// Methods, in ordinal order.
    pub methods: Vec<Method>,
}

/// One library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    /// `a.b`.
    pub name: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// Its transport.
    pub transport: Transport,
    /// Its version.
    pub version: Version,
    /// The `origin-version.toml` key it came from, if any.
    pub version_key: Option<String>,
    /// Libraries it uses, by name.
    pub uses: Vec<String>,
    /// Constants.
    pub consts: Vec<Const>,
    /// Enums.
    pub enums: Vec<Rc<Enum>>,
    /// Bits.
    pub bits: Vec<Rc<Bits>>,
    /// Structs.
    pub structs: Vec<Rc<Struct>>,
    /// Capabilities.
    pub capabilities: Vec<Capability>,
    /// Interfaces.
    pub interfaces: Vec<Interface>,
}

/// A whole compilation: every library given, and the roles (§4.3) they supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compilation {
    /// The libraries, each after every library it uses.
    pub libraries: Vec<Library>,
    /// The `status` enum, if declared.
    pub status: Option<Rc<Enum>>,
    /// The `rights` bits, if declared.
    pub rights: Option<Rc<Bits>>,
    /// The `object_type` enum, if declared.
    pub object_type: Option<Rc<Enum>>,
    /// `max_message_bytes`, if declared.
    pub max_message_bytes: Option<u64>,
    /// `max_message_handles`, if declared.
    pub max_message_handles: Option<u64>,
}
