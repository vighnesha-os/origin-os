//! The syntax tree of one library file, before any name is resolved.

/// A position in a file (1-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    /// Line.
    pub line: u32,
    /// Column.
    pub column: u32,
}

/// A word and its position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    /// The word.
    pub text: String,
    /// Where it starts.
    pub pos: Pos,
}

/// `a.b.c`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dotted {
    /// The components.
    pub parts: Vec<String>,
    /// Where it starts.
    pub pos: Pos,
}

impl Dotted {
    /// `a.b.c`, as written.
    pub fn text(&self) -> String {
        self.parts.join(".")
    }
}

/// `@name` or `@name(args)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    /// The attribute's name.
    pub name: String,
    /// Its arguments.
    pub args: Vec<AttrArg>,
    /// Where it starts.
    pub pos: Pos,
}

/// One attribute argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttrArg {
    /// `1.2`.
    Version(u64, u64),
    /// `a.b`.
    Path(Vec<String>),
}

/// `kernel.read_sizes` or `read_sizes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    /// The last component of a `use`d library, if given.
    pub qualifier: Option<String>,
    /// The item.
    pub item: String,
    /// Where it starts.
    pub pos: Pos,
}

/// A count in a `vector` or `array`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bound {
    /// A number.
    Number(u64, Pos),
    /// The name of an unsigned constant.
    Name(Name),
}

/// A type, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeExpr {
    /// `bool`.
    Bool(Pos),
    /// An integer type or an item.
    Named(Name),
    /// `handle<…>` or `optional<handle<…>>`.
    Handle {
        /// A member of `object_type`, or `any`.
        object: Ident,
        /// Members of `rights`.
        rights: Vec<Ident>,
        /// Whether it may be absent.
        optional: bool,
        /// Where it starts.
        pos: Pos,
    },
    /// `array<T, N>`.
    Array(Box<TypeExpr>, Bound, Pos),
    /// `vector<T, N>`.
    Vector(Box<TypeExpr>, Bound, Pos),
    /// `string<N>`.
    String(Bound, Pos),
}

impl TypeExpr {
    /// Where the type starts.
    pub fn pos(&self) -> Pos {
        match self {
            Self::Bool(pos) | Self::Handle { pos, .. } => *pos,
            Self::Named(name) => name.pos,
            Self::Array(_, _, pos) | Self::Vector(_, _, pos) | Self::String(_, pos) => *pos,
        }
    }
}

/// How a constant's value is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConstValue {
    /// `-5` or `0x10`.
    Number {
        /// Whether it had a leading `-`.
        negative: bool,
        /// Its magnitude.
        value: u64,
        /// Where it starts.
        pos: Pos,
    },
    /// `read | write`.
    Bits(Vec<Ident>),
}

/// `const name: type = value;`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// Name.
    pub name: Ident,
    /// Type.
    pub ty: Name,
    /// Value.
    pub value: ConstValue,
}

/// An `enum` or `bits` member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// Name.
    pub name: Ident,
    /// Whether the value had a leading `-`.
    pub negative: bool,
    /// Magnitude of the value.
    pub value: u64,
}

/// `enum` or `bits` (by the field it is stored in).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValuesDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// Name.
    pub name: Ident,
    /// Underlying integer type.
    pub repr: Name,
    /// Members.
    pub members: Vec<MemberDecl>,
}

/// One field of a struct.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// Name.
    pub name: Ident,
    /// Type.
    pub ty: TypeExpr,
}

/// `struct`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// `sized struct`.
    pub sized: bool,
    /// Name.
    pub name: Ident,
    /// Fields.
    pub fields: Vec<FieldDecl>,
}

/// `capability a.b;`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// Name.
    pub name: Dotted,
}

/// A parameter or a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// Name.
    pub name: Ident,
    /// Type.
    pub ty: TypeExpr,
}

/// What a method gives back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Returns {
    /// Nothing written after `)`.
    None,
    /// `-> never`.
    Never(Pos),
    /// `-> (…)`.
    Values(Vec<ParamDecl>),
}

/// `requires …`
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requires {
    /// `requires none`.
    None,
    /// `requires a.b [else status]`.
    Capability {
        /// The capability.
        name: Dotted,
        /// The refusal status, if not `access_denied`.
        otherwise: Option<Ident>,
    },
}

/// One method.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Attributes.
    pub attrs: Vec<Attr>,
    /// `oneway`.
    pub oneway: bool,
    /// Name.
    pub name: Ident,
    /// The number after `=`.
    pub ordinal: u64,
    /// Parameters.
    pub params: Vec<ParamDecl>,
    /// Results.
    pub returns: Returns,
    /// `errors(…)`, if written.
    pub errors: Option<Vec<Ident>>,
    /// `requires …`.
    pub requires: Requires,
    /// Where the method starts.
    pub pos: Pos,
}

/// One interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceDecl {
    /// Doc comment.
    pub doc: Vec<String>,
    /// Name.
    pub name: Ident,
    /// Methods.
    pub methods: Vec<MethodDecl>,
}

/// One top-level item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// `const`.
    Const(ConstDecl),
    /// `enum`.
    Enum(ValuesDecl),
    /// `bits`.
    Bits(ValuesDecl),
    /// `struct`.
    Struct(StructDecl),
    /// `capability`.
    Capability(CapabilityDecl),
    /// `interface`.
    Interface(InterfaceDecl),
}

/// How a library's version is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionDecl {
    /// `version 1.2;`
    Literal(u64, u64, Pos),
    /// `version compatibility "user-abi";`
    Key(String, Pos),
}

/// The two transports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Registers and pointers to the kernel.
    Syscall,
    /// Messages to a service.
    Channel,
}

/// One library file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFile {
    /// The input's name, for messages.
    pub file: String,
    /// Doc comment.
    pub doc: Vec<String>,
    /// The library's name.
    pub name: Dotted,
    /// Its transport.
    pub transport: Transport,
    /// Its version.
    pub version: VersionDecl,
    /// Libraries it uses.
    pub uses: Vec<Dotted>,
    /// Its items.
    pub items: Vec<Item>,
}
