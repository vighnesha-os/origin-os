//! Every way a definition can be refused (`docs/spec/OIDL.md`). Each rejection is a named
//! [`Code`] with a position, so a test can say exactly why a definition must fail and a mistake
//! in the tool cannot hide behind a generic error (CLAUDE.md §13).

use core::fmt;

macro_rules! codes {
    ($($name:ident = $text:literal,)*) => {
        /// Why a definition was refused.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Code {
            $(
                #[doc = $text]
                $name,
            )*
        }

        impl Code {
            /// Every code, so a test can prove each one has a definition that produces it.
            pub const ALL: &'static [Code] = &[$(Code::$name,)*];

            /// The code's short description.
            pub const fn text(self) -> &'static str {
                match self {
                    $(Code::$name => $text,)*
                }
            }
        }
    };
}

codes! {
    // Lexing (§2).
    SourceTooLarge = "the file is larger than 1 MiB",
    NotUtf8 = "the file is not valid UTF-8",
    ForbiddenCharacter = "a character that is not allowed here",
    LineTooLong = "a line is longer than 4096 bytes",
    TooManyTokens = "the file has more than 200 000 tokens",
    BadNumber = "a malformed number",
    NumberTooLarge = "a number does not fit in 64 bits",
    BadString = "a malformed string",
    BadIdentifier = "an identifier breaks the naming rules",
    IdentifierTooLong = "an identifier is longer than 64 bytes",
    ForbiddenDocText = "a doc comment contains */, /*, or a code fence",
    ReservedWord = "a name is reserved by Rust, C or the generator",
    // Parsing (§2.1).
    UnexpectedToken = "something else was expected here",
    MissingDoc = "a declaration must carry a doc comment",
    NestingTooDeep = "types are nested more than 8 levels, or structs more than 16",
    TooMany = "a limit on how many items a declaration may hold is exceeded",
    BadVersion = "a malformed version",
    // Libraries and names.
    DuplicateLibrary = "two inputs define the same library",
    BadLibraryName = "a library name needs 2 to 4 components",
    UnknownLibrary = "use of a library that was not given",
    UseCycle = "libraries use each other in a cycle",
    DuplicateUse = "the same library is used twice, or two have the same last name",
    VersionKeyMissing = "a version names a compatibility key but no version file was given",
    VersionKeyUnknown = "the version file has no such compatibility key",
    VersionKeyMalformed = "the compatibility key's value is not a version",
    DuplicateName = "two items in this library have the same name",
    UnknownName = "no such item",
    WrongKind = "this name is the wrong kind of item here",
    NameCollision = "two names collide once converted for Rust or C",
    // Roles (§4.3).
    DuplicateRole = "status, rights, object_type or a limit is declared twice",
    BadRole = "status, rights or object_type does not have its required form",
    MissingRole = "this library needs status, rights, object_type or a limit, and has no access to it",
    // Constants, enums, bits.
    BadConstType = "a constant must be an integer or bits type",
    ConstOutOfRange = "a constant's value does not fit its type",
    BadConstValue = "a constant's value is not of its type",
    EmptyDeclaration = "a declaration has no members",
    DuplicateValue = "two members have the same value",
    ValueOutOfRange = "a member's value does not fit",
    BitIndexOutOfRange = "a bit index is outside the type",
    BadUnderlying = "an enum or bits declaration needs a permitted integer type",
    ReservedMember = "a bits member may not be named all or none",
    // Structures and types.
    RecursiveStruct = "a struct contains itself",
    BadPosition = "this type may not appear in this position",
    FieldNamedSize = "a sized struct may not have a field named size",
    SizedNotAllowed = "a sized struct is only for a syscall library",
    BoundOutOfRange = "a bound is out of range or not an unsigned constant",
    TooLarge = "a size exceeds the 64 KiB limit",
    SinceNotIncreasing = "declaration order must not decrease in version",
    BadSince = "a version in @since is not valid here",
    NewerReference = "from major 1, an item names an item of its library from a later minor",
    BadHandleType = "unknown object type or right in a handle type",
    // Interfaces and methods.
    DuplicateOrdinal = "two methods have the same ordinal",
    BadOrdinal = "an ordinal is out of range",
    DuplicateParam = "two parameters of a method have the same name",
    MissingErrors = "a method must have an errors clause",
    ErrorsNotAllowed = "this method may not have an errors clause",
    NotAStatus = "an error is not a member of status",
    DuplicateError = "the same error is listed twice",
    OkInErrors = "ok is not an error",
    UndeclaredDerivedError = "a status this method's decoding can produce is not declared",
    UnknownCapability = "no such capability",
    DuplicateCapability = "the same capability is declared twice",
    ElseNotAllowed = "else is only for a syscall interface",
    DeniedNotDeclared = "the refusal status is not in the method's errors",
    OnewayNotAllowed = "oneway is only for a channel interface and has no results",
    NeverNotAllowed = "-> never is only for a syscall interface",
    ReservedNotAllowed = "@reserved is only for a method of a syscall interface",
    ReservedNeedsNotSupported = "a @reserved method must declare not_supported",
    TooManyRegisters = "a method needs more than six registers",
    MessageTooLarge = "a method's largest message exceeds max_message_bytes",
    TooManyHandles = "a method's messages can exceed max_message_handles",
    // Attributes (§4.4).
    UnknownAttribute = "no such attribute",
    MisplacedAttribute = "this attribute is not allowed here",
    DuplicateAttribute = "the same attribute is given twice",
    BadAttributeArgs = "this attribute's arguments are wrong",
    CountRequired = "a vector or string result of a syscall needs @count",
    BadCount = "@count does not name a u32 field of a struct result",
    AlsoOnNotDeclared = "@also_on names an error the method does not declare",
    NonzeroType = "@nonzero needs an integer or bits parameter",
}

/// A refusal: what, why and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// The reason.
    pub code: Code,
    /// The input it came from.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
    /// What exactly is wrong.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}: {}",
            self.file,
            self.line,
            self.column,
            self.code.text(),
            self.message
        )
    }
}

impl std::error::Error for Diagnostic {}
