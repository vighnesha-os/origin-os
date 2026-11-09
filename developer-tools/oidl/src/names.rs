//! Names in generated code (`docs/spec/OIDL.md` §2). Every OIDL name is lower-case words; the
//! generators turn it into `PascalCase` or `SHOUTY_CASE` mechanically, so a name that is a
//! keyword of a target language, or that would shadow what generated code relies on, is refused
//! at the source rather than escaped.

/// Rust 2024 keywords and reserved words, C keywords through C23, C++ keywords through C++26,
/// and the words the generators use for themselves. Sorted, so the check is a binary search.
const RESERVED: &[&str] = &[
    "abstract",
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "as",
    "asm",
    "async",
    "atomic_bool",
    "auto",
    "await",
    "become",
    "bitand",
    "bitor",
    "bool",
    "box",
    "break",
    "case",
    "catch",
    "char",
    "char16_t",
    "char32_t",
    "char8_t",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "compl",
    "concept",
    "const",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "continue",
    "contract_assert",
    "crate",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dyn",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "final",
    "float",
    "fn",
    "for",
    "friend",
    "gen",
    "goto",
    "if",
    "impl",
    "in",
    "inline",
    "int",
    "let",
    "long",
    "loop",
    "macro",
    "match",
    "mod",
    "move",
    "mut",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "override",
    "priv",
    "private",
    "protected",
    "pub",
    "public",
    "ref",
    "register",
    "reinterpret_cast",
    "requires",
    "restrict",
    "return",
    "self",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "super",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "trait",
    "true",
    "try",
    "type",
    "typedef",
    "typeid",
    "typename",
    "typeof",
    "typeof_unqual",
    "union",
    "unsafe",
    "unsigned",
    "unsized",
    "use",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "where",
    "while",
    "xor",
    "xor_eq",
    "yield",
];

/// `PascalCase` names generated code must be able to refer to, or that it defines itself.
const RESERVED_PASCAL: &[&str] = &[
    "Calls",
    "Capability",
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Dispatched",
    "Drop",
    "Eq",
    "Err",
    "Error",
    "Fixed",
    "Fn",
    "FnMut",
    "FnOnce",
    "From",
    "Handle",
    "HandleArg",
    "HandleList",
    "HandleSink",
    "HandleSlice",
    "Hash",
    "Into",
    "Iterator",
    "MethodError",
    "None",
    "Ok",
    "Option",
    "Ord",
    "Outcome",
    "PartialEq",
    "PartialOrd",
    "PhantomData",
    "Reader",
    "RecvHandle",
    "Result",
    "Scratch",
    "Self",
    "Send",
    "Sized",
    "Some",
    "Status",
    "String",
    "Sync",
    "Transport",
    "UserBuffer",
    "UserRef",
    "UserSlice",
    "Vec",
    "View",
    "WireError",
    "Writer",
];

/// Whether `word` may not name anything: a keyword of a target language, or a name the
/// generators keep for themselves. OIDL's own keywords that are not in this list (`handle`,
/// `version`, `string`, `method` and the like) are contextual, so they stay usable as parameter
/// and field names.
pub fn is_reserved(word: &str) -> bool {
    word.starts_with("oidl_") || RESERVED.binary_search(&word).is_ok()
}

/// Whether `word` is a type word, which an item may not be named because the type syntax takes
/// it first.
pub fn is_type_word(word: &str) -> bool {
    matches!(
        word,
        "bool"
            | "handle"
            | "optional"
            | "array"
            | "vector"
            | "string"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
    )
}

/// Whether a `PascalCase` name is taken by what generated code uses.
pub fn is_reserved_pascal(name: &str) -> bool {
    RESERVED_PASCAL.contains(&name)
}

/// `read_sizes` → `ReadSizes`.
pub fn pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for part in name.split('_') {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    out
}

/// `read_sizes` → `READ_SIZES`.
pub fn shouty(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// `origin.kernel` → `origin_kernel`, the Rust module and C prefix of a library.
pub fn module(library: &str) -> String {
    library.replace('.', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_lists_are_sorted_and_cover_each_language() {
        let mut sorted = RESERVED.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, RESERVED, "binary search needs a sorted list");
        for word in [
            "match",
            "gen",
            "register",
            "typedef",
            "class",
            "template",
            "public",
            "typeof_unqual",
            "consteval",
            "constinit",
            "contract_assert",
            "oidl_x",
        ] {
            assert!(is_reserved(word), "{word}");
        }
        for word in [
            "handle", "version", "bytes", "signals", "rights", "deadline", "end",
        ] {
            assert!(!is_reserved(word), "{word}");
        }
    }

    #[test]
    fn names_convert_mechanically() {
        assert_eq!(pascal("read_sizes"), "ReadSizes");
        assert_eq!(pascal("foo_1"), "Foo1");
        assert_eq!(
            pascal("foo1"),
            "Foo1",
            "a collision the validator must catch"
        );
        assert_eq!(shouty("read_sizes"), "READ_SIZES");
        assert_eq!(module("origin.kernel"), "origin_kernel");
        assert!(is_reserved_pascal("Result"));
        assert!(!is_reserved_pascal("ReadSizes"));
    }
}
