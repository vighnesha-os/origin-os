//! Why a table was refused: which table, where in it, and which rule.

use core::fmt;

/// A table's four-byte signature, as the firmware wrote it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Signature(pub [u8; 4]);

impl Signature {
    /// The signature `bytes`.
    #[must_use]
    pub const fn new(bytes: &[u8; 4]) -> Self {
        Self(*bytes)
    }

    /// Whether the signature is `bytes`.
    #[must_use]
    pub fn is(self, bytes: &[u8; 4]) -> bool {
        self.0 == *bytes
    }
}

impl fmt::Display for Signature {
    /// Printable ASCII as itself, anything else as `?`: a signature is firmware data, and a
    /// console line must not carry control characters from it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &b in &self.0 {
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

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{self}\"")
    }
}

/// A refusal: the table, the byte offset in the table where the fault is, and the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    /// The table whose bytes were refused.
    pub table: Signature,
    /// The offset, from the table's first byte, of the field or structure at fault.
    pub at: usize,
    /// The rule the bytes break.
    pub kind: ErrorKind,
}

/// The rule a refused table breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The table's signature is not the one its caller, or the pointer to it, expects.
    Signature {
        /// The signature found.
        found: Signature,
    },
    /// The header's length is below the header itself, or is not the number of bytes given.
    Length {
        /// The length the header declares.
        declared: u64,
        /// The bytes the caller gave.
        available: usize,
    },
    /// The table's revision is one whose layout ACPI 6.6 no longer gives, so it cannot be read.
    Revision(u8),
    /// The table is shorter than the fields its revision defines.
    TooShort {
        /// The length those fields need.
        need: usize,
    },
    /// The table's bytes do not sum to zero.
    Checksum,
    /// A field or structure runs past the end of the table, or of the structure that holds it.
    Truncated {
        /// The offset, from the table's first byte, that the field would end at.
        need: usize,
    },
    /// A structure's length is zero or below the minimum for its type.
    Structure {
        /// The structure's type.
        kind: u16,
        /// The length it declares.
        length: usize,
    },
    /// An offset or count points outside the table, into its header, or past its parent.
    Offset {
        /// The field.
        field: &'static str,
    },
    /// A field holds a value that ACPI 6.6 forbids, or reserves where acting on it would be wrong.
    /// Each such refusal is listed in the crate's documentation.
    Value {
        /// The field.
        field: &'static str,
    },
    /// Two fields that ACPI 6.6 requires to agree do not.
    Inconsistent {
        /// The field found to disagree.
        field: &'static str,
    },
    /// A reference names no structure, or one of the wrong type.
    Reference {
        /// The field holding the reference.
        field: &'static str,
    },
    /// A chain of parent references comes back to where it started.
    Cycle,
}

impl Error {
    /// A refusal of `table` at `at`.
    #[must_use]
    pub const fn new(table: Signature, at: usize, kind: ErrorKind) -> Self {
        Self { table, at, kind }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Signature { found } => write!(f, "signature {found} where another was expected"),
            Self::Length {
                declared,
                available,
            } => write!(f, "length {declared} with {available} bytes given"),
            Self::Revision(r) => write!(f, "revision {r}, whose layout is not ACPI 6.6's"),
            Self::TooShort { need } => write!(f, "too short for its revision: needs {need} bytes"),
            Self::Checksum => f.write_str("checksum"),
            Self::Truncated { need } => write!(f, "runs past the end, to {need:#x}"),
            Self::Structure { kind, length } => {
                write!(f, "structure type {kind} with length {length}")
            }
            Self::Offset { field } => write!(f, "{field} points outside its table"),
            Self::Value { field } => write!(f, "{field} holds a value it may not"),
            Self::Inconsistent { field } => write!(f, "{field} disagrees with another field"),
            Self::Reference { field } => write!(f, "{field} names no structure of its kind"),
            Self::Cycle => f.write_str("its parents form a cycle"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {:#x}: {}", self.table, self.at, self.kind)
    }
}
