//! Known-good and malformed definitions (`docs/spec/OIDL.md` §2–§10). Every [`Code`] has at
//! least one definition that produces it: [`every_rejection_has_a_definition_that_produces_it`]
//! fails if a new rejection is added without one.

use std::collections::HashSet;

use crate::diag::Code;
use crate::layout;
use crate::lex;
use crate::sema::{Input, compile};

/// A library that supplies every role (§4.3), for the cases below to `use`.
const ROLES: &str = r#"
/// roles
library test.kernel;
transport syscall;
version 1.0;
/// status
enum status: i64 {
    ok = 0; invalid_args = -1; bad_handle = -2; wrong_type = -3; access_denied = -4;
    no_memory = -5; fault = -6; should_wait = -7; not_supported = -11; limit = -12;
}
/// rights
bits rights: u32 { read = 0; write = 1; transfer = 2; }
/// object_type
enum object_type: u32 { channel = 1; event = 2; }
/// bytes
const max_message_bytes: u32 = 65536;
/// handles
const max_message_handles: u32 = 64;
"#;

fn compile_all(sources: &[(&str, String)], versions: Option<&str>) -> Result<(), Code> {
    let inputs: Vec<Input> = sources
        .iter()
        .map(|(name, text)| Input {
            file: name.to_string(),
            text: text.as_bytes().to_vec(),
        })
        .collect();
    compile(&inputs, versions).map(|_| ()).map_err(|d| d.code)
}

fn roles() -> (&'static str, String) {
    ("roles", ROLES.to_string())
}

/// A `syscall` library with `items`, at version 1.2.
fn sys(items: &str) -> Vec<(&'static str, String)> {
    vec![
        roles(),
        (
            "case",
            format!(
                "/// lib\nlibrary test.sys;\ntransport syscall;\nversion 1.2;\nuse test.kernel;\n{items}\n"
            ),
        ),
    ]
}

/// A `channel` library with `items`, at version 1.2.
fn chan(items: &str) -> Vec<(&'static str, String)> {
    vec![
        roles(),
        (
            "case",
            format!(
                "/// lib\nlibrary test.chan;\ntransport channel;\nversion 1.2;\nuse test.kernel;\n{items}\n"
            ),
        ),
    ]
}

/// One malformed input: raw bytes or definitions, and the code it must produce.
#[derive(Debug)]
enum Case {
    Bytes(Vec<u8>),
    Sources(Vec<(&'static str, String)>),
    WithVersions(Vec<(&'static str, String)>, Option<&'static str>),
}

fn run(case: &Case) -> Result<(), Code> {
    match case {
        Case::Bytes(bytes) => {
            let input = Input {
                file: "bytes".to_string(),
                text: bytes.clone(),
            };
            compile(&[input], None).map(|_| ()).map_err(|d| d.code)
        }
        Case::Sources(sources) => compile_all(sources, None),
        Case::WithVersions(sources, versions) => compile_all(sources, *versions),
    }
}

fn s(text: impl Into<String>) -> Case {
    let text = text.into();
    Case::Bytes(text.into_bytes())
}

fn one(method: &str) -> String {
    format!("/// i\ninterface i {{\n/// m\n{method}\n}}")
}

/// `count` structs, each holding the previous: `s0 { a: u8 }`, `s1 { a: s0 }`, …
fn chain_of_structs(count: usize) -> String {
    let mut text = String::from("/// s\nstruct s0 { a: u8; }\n");
    for i in 1..count {
        text.push_str(&format!("/// s\nstruct s{i} {{ a: s{}; }}\n", i - 1));
    }
    text
}

fn cases() -> Vec<(Code, Case)> {
    use Code::*;
    let long_name = "a".repeat(65);
    let deep: String = {
        let mut t = "u8".to_string();
        for _ in 0..10 {
            t = format!("array<{t}, 2>");
        }
        t
    };
    vec![
        (SourceTooLarge, Case::Bytes(vec![b' '; lex::MAX_SOURCE + 1])),
        (NotUtf8, Case::Bytes(vec![0xFF, 0xFE])),
        (ForbiddenCharacter, s("/// d\nlibrary a.b;\n\ttransport syscall;")),
        (ForbiddenCharacter, s("/// d\r\nlibrary a.b;")),
        (ForbiddenCharacter, s("// a comment with a control \u{0007} char\n")),
        (LineTooLong, s(format!("// {}\n", "x".repeat(4100)))),
        (LineTooLong, s(format!("/// {}", "x".repeat(4100)))),
        (TooManyTokens, s(";".repeat(lex::MAX_TOKENS + 1))),
        (BadNumber, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst c: u32 = 1_;")),
        (BadNumber, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst c: u32 = 012;")),
        (BadNumber, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst c: u32 = 0x;")),
        (BadNumber, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst c: u32 = 0Xff;")),
        (BadNumber, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst c: u32 = 12ab;")),
        (NumberTooLarge, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst c: u64 = 18446744073709551616;")),
        (NumberTooLarge, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst c: u64 = 0x1_0000_0000_0000_0000;")),
        (BadString, s("/// d\nlibrary a.b; transport syscall; version compatibility \"a\\b\";")),
        (BadString, s("/// d\nlibrary a.b; transport syscall; version compatibility \"unterminated;")),
        (BadString, s(format!("/// d\nlibrary a.b; transport syscall; version compatibility \"{}\";", "k".repeat(65)))),
        (BadIdentifier, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst Foo: u32 = 1;")),
        (BadIdentifier, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst foo_: u32 = 1;")),
        (BadIdentifier, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst foo__bar: u32 = 1;")),
        (BadIdentifier, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst fooBar: u32 = 1;")),
        (BadIdentifier, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst _foo: u32 = 1;")),
        (IdentifierTooLong, s(format!("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst {long_name}: u32 = 1;"))),
        (ForbiddenDocText, s("/// a */ b\nlibrary a.b;")),
        (ForbiddenDocText, s("/// a /* b\nlibrary a.b;")),
        (ForbiddenDocText, s("/// ```\nlibrary a.b;")),
        (ForbiddenDocText, s("/// ~~~\nlibrary a.b;")),
        (ReservedWord, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst match: u32 = 1;")),
        (ReservedWord, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst oidl_x: u32 = 1;")),
        (ReservedWord, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst public: u32 = 1;")),
        // Keywords only from C23, C++20 and C++26: a name the older compilers accept.
        (ReservedWord, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst typeof_unqual: u32 = 1;")),
        (ReservedWord, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst consteval: u32 = 1;")),
        (ReservedWord, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst constinit: u32 = 1;")),
        (ReservedWord, s("/// d\nlibrary a.b; transport syscall; version 1.2; /// c\nconst contract_assert: u32 = 1;")),
        (ReservedWord, Case::Sources(sys("/// c\nconst vector: u32 = 1;"))),
        (ReservedWord, Case::Sources(sys("/// c\nconst u32: u32 = 1;"))),
        (UnexpectedToken, s("/// d\nlibrary ;")),
        (UnexpectedToken, s("/// d\nlibrary a.b; transport tcp;")),
        (UnexpectedToken, s("/// d\nlibrary a.b; transport syscall; version 1.2;\n/// c\nstray")),
        (UnexpectedToken, Case::Sources(sys("/// c\nconst x u32 = 1;"))),
        (UnexpectedToken, Case::Sources(sys("/// e\nenum e: u8 { a = ; }"))),
        (UnexpectedToken, Case::Sources(sys(&one("method m = 1 (x u32) requires none;")))),
        (UnexpectedToken, Case::Sources(sys(&one("method m = 1 () errors(a b) requires none;")))),
        (UnexpectedToken, Case::Sources(sys("/// c\nconst x: u32 = -;"))),
        (MissingDoc, s("library a.b; transport syscall; version 1.2;")),
        (MissingDoc, s("/// d\nlibrary a.b; transport syscall; version 1.2;\nconst x: u32 = 1;")),
        (MissingDoc, Case::Sources(sys("/// i\ninterface i {\nmethod m = 1 () errors() requires none;\n}"))),
        (NestingTooDeep, Case::Sources(sys(&format!("/// s\nstruct s {{ a: {deep}; }}")))),
        (NestingTooDeep, Case::Sources(sys(&chain_of_structs(18)))),
        (TooMany, Case::Bytes((0..=crate::parse::MAX_ITEMS).fold(String::from("/// d\nlibrary a.b; transport syscall; version 1.0;\n"), |mut t, i| { t.push_str(&format!("/// c\nconst c{i}: u32 = 1;\n")); t }).into_bytes())),
        (TooMany, Case::Bytes(format!("/// d\nlibrary a.b; transport syscall; version 1.0;{}", "use c.d;\n".repeat(crate::parse::MAX_USES + 1)).into_bytes())),
        (TooMany, Case::Sources(sys(&format!("/// e\nenum e: u16 {{ {} }}", (0..=crate::parse::MAX_MEMBERS).map(|i| format!("m{i} = {i};\n")).collect::<String>())))),
        (TooMany, Case::Sources(sys(&format!("/// s\nstruct s {{ {} }}", (0..=crate::parse::MAX_FIELDS).map(|i| format!("f{i}: u8;\n")).collect::<String>())))),
        (TooMany, Case::Sources(sys(&format!("/// i\ninterface i {{\n{} }}", (0..=crate::parse::MAX_METHODS).map(|i| format!("/// m\nmethod m{i} = {i} () errors() requires none;\n")).collect::<String>())))),
        (TooMany, Case::Sources(sys(&one(&format!("method m = 1 ({}) errors() requires none;", (0..=crate::parse::MAX_PARAMS).map(|i| format!("p{i}: u64,\n")).collect::<String>()))))),
        (TooMany, Case::Sources(sys(&format!("/// e\nenum e: u8 {{ {} a = 1; }}", "@since(1.1)\n".repeat(crate::parse::MAX_ATTRS + 1))))),
        (BadVersion, s("/// d\nlibrary a.b; transport syscall; version x;")),
        (BadVersion, s("/// d\nlibrary a.b; transport syscall; version 70000.0;")),
        (DuplicateLibrary, Case::Sources(vec![
            ("one", "/// d\nlibrary a.b; transport syscall; version 1.0;\n".to_string()),
            ("two", "/// d\nlibrary a.b; transport syscall; version 1.0;\n".to_string()),
        ])),
        (BadLibraryName, s("/// d\nlibrary a; transport syscall; version 1.0;")),
        (BadLibraryName, s("/// d\nlibrary a.b.c.d.e; transport syscall; version 1.0;")),
        (BadLibraryName, Case::Sources(sys("/// c\ncapability single;"))),
        (UnknownLibrary, s("/// d\nlibrary a.b; transport syscall; version 1.0; use c.d;")),
        (UseCycle, Case::Sources(vec![
            ("one", "/// d\nlibrary a.a; transport syscall; version 1.0; use a.b;\n".to_string()),
            ("two", "/// d\nlibrary a.b; transport syscall; version 1.0; use a.a;\n".to_string()),
        ])),
        (UseCycle, Case::Sources(vec![
            ("one", "/// d\nlibrary a.a; transport syscall; version 1.0; use a.a;\n".to_string()),
        ])),
        (DuplicateUse, Case::Sources(vec![
            roles(),
            ("two", "/// d\nlibrary a.b; transport syscall; version 1.0; use test.kernel; use test.kernel;\n".to_string()),
        ])),
        (DuplicateUse, Case::Sources(vec![
            ("x", "/// d\nlibrary p.same; transport syscall; version 1.0;\n".to_string()),
            ("y", "/// d\nlibrary q.same; transport syscall; version 1.0;\n".to_string()),
            ("z", "/// d\nlibrary r.user; transport syscall; version 1.0; use p.same; use q.same;\n".to_string()),
        ])),
        (VersionKeyMissing, Case::WithVersions(vec![
            ("one", "/// d\nlibrary a.b; transport syscall; version compatibility \"user-abi\";\n".to_string()),
        ], None)),
        (VersionKeyUnknown, Case::WithVersions(vec![
            ("one", "/// d\nlibrary a.b; transport syscall; version compatibility \"nope\";\n".to_string()),
        ], Some("[compatibility]\nuser-abi = \"0.1\"\n"))),
        (VersionKeyMalformed, Case::WithVersions(vec![
            ("one", "/// d\nlibrary a.b; transport syscall; version compatibility \"user-abi\";\n".to_string()),
        ], Some("[compatibility]\nuser-abi = \"zero\"\n"))),
        (DuplicateName, Case::Sources(sys("/// c\nconst a: u32 = 1;\n/// c\nconst a: u32 = 2;"))),
        (DuplicateName, Case::Sources(sys("/// e\nenum e: u8 { a = 1; a = 2; }"))),
        (DuplicateName, Case::Sources(sys("/// b\nbits b: u8 { a = 1; a = 2; }"))),
        (DuplicateName, Case::Sources(sys("/// s\nstruct s { a: u8; a: u16; }"))),
        (DuplicateName, Case::Sources(sys("/// i\ninterface i {\n/// m\nmethod a = 1 () errors() requires none;\n/// n\nmethod a = 2 () errors() requires none;\n}"))),
        (UnknownName, Case::Sources(sys("/// s\nstruct s { a: nothing; }"))),
        (UnknownName, Case::Sources(sys("/// s\nstruct s { a: other.thing; }"))),
        (UnknownName, Case::Sources(sys(&one("method m = 1 (a: vector<u8, nothing>) errors(invalid_args, fault) requires none;")))),
        (WrongKind, Case::Sources(sys("/// s\nstruct s { a: i; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () errors() requires none;\n}"))),
        (WrongKind, Case::Sources(sys("/// c\nconst c: u32 = 1;\n/// s\nstruct s { a: c; }"))),
        (WrongKind, Case::Sources(sys("/// t\nstruct t { x: u8; }\n/// s\nstruct s { a: array<u8, t>; }"))),
        (NameCollision, Case::Sources(sys("/// a\nstruct foo_1 { x: u8; }\n/// b\nstruct foo1 { x: u8; }"))),
        (NameCollision, Case::Sources(sys("/// a\nconst foo_bar: u32 = 1;\n/// b\nconst foo_b_ar: u32 = 1;\n/// c\nenum foo: u8 { bar = 1; }\n/// d\nenum foo_b: u8 { ar = 1; }"))),
        (NameCollision, Case::Sources(sys("/// r\nstruct error { x: u8; }"))),
        (NameCollision, Case::Sources(sys("/// e\nenum e: u8 { a_1 = 1; a1 = 2; }"))),
        (NameCollision, Case::Sources(sys("/// i\ninterface types {\n/// m\nmethod m = 1 () errors() requires none;\n}"))),
        (NameCollision, Case::Sources(sys(&one("method holds = 1 () errors() requires none;")))),
        (NameCollision, Case::Sources(sys(&one("method transport = 1 () errors() requires none;")))),
        (DuplicateRole, Case::Sources(sys("/// s\nenum status: i64 { ok = 0; }"))),
        (DuplicateRole, Case::Sources(sys("/// r\nbits rights: u32 { a = 0; }"))),
        (DuplicateRole, Case::Sources(sys("/// o\nenum object_type: u32 { a = 1; }"))),
        (DuplicateRole, Case::Sources(sys("/// b\nconst max_message_bytes: u32 = 1;"))),
        (DuplicateRole, Case::Sources(sys("/// h\nconst max_message_handles: u32 = 1;"))),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// s\nenum status: i32 { ok = 0; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// s\nenum status: i64 { ok = 1; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// s\nenum status: i64 { ok = 0; bad = 1; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// s\nenum status: i64 { bad = -1; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// r\nbits rights: u16 { a = 0; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// o\nenum object_type: u16 { a = 1; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// o\nenum object_type: u32 { any = 1; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// o\nenum object_type: u32 { zero = 0; }")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// r\nbits flagset: u32 { a = 0; }\n/// c\nconst max_message_bytes: flagset = a;")),
        (BadRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// r\nbits flagset: u32 { a = 0; }\n/// c\nconst max_message_handles: flagset = a;")),
        (MissingRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// i\ninterface i {\n/// m\nmethod m = 1 () errors(bad) requires none;\n}")),
        (MissingRole, s("/// d\nlibrary a.b; transport syscall; version 1.0;\n/// i\ninterface i {\n/// m\nmethod m = 1 (h: handle<any>) errors() requires none;\n}")),
        (MissingRole, s("/// d\nlibrary a.b; transport channel; version 1.0;\n")),
        (MissingRole, Case::Sources(vec![
            roles(),
            ("two", "/// d\nlibrary a.b; transport channel; version 1.0;\n/// i\ninterface i {\n/// m\nmethod m = 1 (h: handle<any>) errors() requires none;\n}\n".to_string()),
        ])),
        (BadConstType, Case::Sources(sys("/// c\nconst c: bool = 1;"))),
        (BadConstType, Case::Sources(sys("/// s\nstruct t { x: u8; }\n/// c\nconst c: t = 1;"))),
        (ConstOutOfRange, Case::Sources(sys("/// c\nconst c: u8 = 256;"))),
        (ConstOutOfRange, Case::Sources(sys("/// c\nconst c: u8 = -1;"))),
        (ConstOutOfRange, Case::Sources(sys("/// c\nconst c: i8 = 128;"))),
        (BadConstValue, Case::Sources(sys("/// b\nbits b: u8 { a = 0; }\n/// c\nconst c: b = nope;"))),
        (BadConstValue, Case::Sources(sys("/// b\nbits b: u8 { a = 0; }\n/// c\nconst c: b = 1;"))),
        (BadConstValue, Case::Sources(sys("/// c\nconst c: u32 = a | b;"))),
        (EmptyDeclaration, Case::Sources(sys("/// e\nenum e: u8 { }"))),
        (EmptyDeclaration, Case::Sources(sys("/// b\nbits b: u8 { }"))),
        (EmptyDeclaration, Case::Sources(sys("/// s\nstruct s { }"))),
        (EmptyDeclaration, Case::Sources(sys("/// i\ninterface i { }"))),
        (DuplicateValue, Case::Sources(sys("/// e\nenum e: u8 { a = 1; b = 1; }"))),
        (DuplicateValue, Case::Sources(sys("/// b\nbits b: u8 { a = 1; b = 1; }"))),
        (ValueOutOfRange, Case::Sources(sys("/// e\nenum e: u8 { a = 256; }"))),
        (ValueOutOfRange, Case::Sources(sys("/// e\nenum e: u8 { a = -1; }"))),
        (ValueOutOfRange, Case::Sources(sys("/// e\nenum e: i8 { a = -129; }"))),
        (BitIndexOutOfRange, Case::Sources(sys("/// b\nbits b: u8 { a = 8; }"))),
        (BitIndexOutOfRange, Case::Sources(sys("/// b\nbits b: u8 { a = -1; }"))),
        (BadUnderlying, Case::Sources(sys("/// e\nenum e: bool { a = 1; }"))),
        (BadUnderlying, Case::Sources(sys("/// b\nbits b: i32 { a = 1; }"))),
        (BadUnderlying, Case::Sources(sys("/// e\nenum e: other.u8 { a = 1; }"))),
        (ReservedMember, Case::Sources(sys("/// b\nbits b: u8 { all = 0; }"))),
        (ReservedMember, Case::Sources(sys("/// b\nbits b: u8 { none = 0; }"))),
        (RecursiveStruct, Case::Sources(sys("/// s\nstruct s { a: s; }"))),
        (RecursiveStruct, Case::Sources(sys("/// s\nstruct a { x: array<b, 2>; }\n/// t\nstruct b { y: a; }"))),
        (BadPosition, Case::Sources(sys("/// s\nstruct s { a: vector<u8, 3>; }"))),
        (BadPosition, Case::Sources(sys("/// s\nstruct s { a: string<3>; }"))),
        (BadPosition, Case::Sources(sys("/// s\nstruct s { a: handle<any>; }"))),
        (BadPosition, Case::Sources(sys("/// s\nstruct s { a: kernel.status; }"))),
        (BadPosition, Case::Sources(chan(&one("method m = 1 (v: vector<vector<u8, 2>, 2>) errors() requires none;")))),
        (BadPosition, Case::Sources(chan(&one("method m = 1 (a: array<handle<any>, 2>) errors() requires none;")))),
        (BadPosition, Case::Sources(chan(&one("method m = 1 (a: array<string<3>, 2>) errors() requires none;")))),
        (BadPosition, Case::Sources(chan("/// s\nstruct s { x: u8; }\n/// t\nstruct t { y: vector<s, 2>; }"))),
        (BadPosition, Case::Sources(chan(&one("method m = 1 (a: vector<optional<handle<any>>, 2>) errors() requires none;")))),
        (BadPosition, Case::Sources(sys(&one("method m = 1 (a: vector<bool, 2>) errors(invalid_args, fault) requires none;")))),
        (BadPosition, Case::Sources(sys(&one("method m = 1 (a: array<bool, 2>) errors(fault) requires none;")))),
        // A system-call vector or array element may be a plain struct of integers, enums and
        // bits only (ADR-0005 C3): a bool, a nested struct or an array in it is refused.
        (BadPosition, Case::Sources(sys("/// t\nstruct t { x: bool; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (a: vector<t, 2>) errors(invalid_args, fault) requires none;\n}"))),
        (BadPosition, Case::Sources(sys("/// u\nstruct u { y: u8; }\n/// t\nstruct t { x: u; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (a: vector<t, 2>) errors(invalid_args, fault) requires none;\n}"))),
        (BadPosition, Case::Sources(sys("/// t\nstruct t { x: array<u8, 2>; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () -> (a: array<t, 2>) errors(fault) requires none;\n}"))),
        (BadPosition, Case::Sources(sys("/// t\nstruct t { x: bool; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (a: array<t, 2>) errors(fault) requires none;\n}"))),
        (BadPosition, Case::Sources(chan(&one("method m = 1 (e: kernel.status) errors() requires none;")))),
        // A handle array crosses with no way to state its rights (§3.1).
        (BadPosition, Case::Sources(sys(&one("method m = 1 (a: array<handle<channel, read>, 2>) errors(fault, bad_handle, access_denied, wrong_type) requires none;")))),
        // A method that cannot fail cannot refuse its arguments.
        (BadPosition, Case::Sources(sys(&one("method m = 1 (a: i64) -> never requires none;")))),
        (FieldNamedSize, Case::Sources(sys("/// s\nsized struct s { size: u32; }"))),
        (SizedNotAllowed, Case::Sources(chan("/// s\nsized struct s { a: u32; }"))),
        (BoundOutOfRange, Case::Sources(sys("/// s\nstruct s { a: array<u8, 0>; }"))),
        (BoundOutOfRange, Case::Sources(sys("/// s\nstruct s { a: array<u8, 65537>; }"))),
        (BoundOutOfRange, Case::Sources(sys(&one("method m = 1 (a: vector<u8, 0>) errors(invalid_args, fault) requires none;")))),
        (BoundOutOfRange, Case::Sources(sys(&one("method m = 1 (a: vector<u8, 4294967296>) errors(invalid_args, fault) requires none;")))),
        (BoundOutOfRange, Case::Sources(sys("/// c\nconst c: i32 = 3;\n/// s\nstruct s { a: array<u8, c>; }"))),
        (TooLarge, Case::Sources(sys("/// s\nstruct s { a: array<u32, 20000>; }"))),
        // A system-call vector of structs is encoded into a buffer of its bound: 64 KiB at most.
        (TooLarge, Case::Sources(sys("/// t\nstruct t { x: u32; y: u32; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (a: vector<t, 8193>) errors(invalid_args, fault) requires none;\n}"))),
        (TooLarge, Case::Sources(sys("/// s\nstruct t { x: array<u64, 8192>; }\n/// t\nstruct s { a: array<t, 2>; }"))),
        (SinceNotIncreasing, Case::Sources(sys("/// s\nsized struct s { @since(1.2) a: u32; @since(1.1) b: u32; }"))),
        // From major 1 nothing names an item of its library from a later minor (§9): a
        // parameter, an array element, a field, a constant's bits and members, a capability and
        // a status.
        (NewerReference, Case::Sources(sys("/// e\n@since(1.2) enum e: u8 { a = 1; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: e) errors(invalid_args) requires none;\n}"))),
        (NewerReference, Case::Sources(sys("/// t\n@since(1.1) struct t { x: u8; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () -> (a: array<t, 2>) errors(fault) requires none;\n}"))),
        (NewerReference, Case::Sources(sys("/// e\n@since(1.2) enum e: u8 { a = 1; }\n/// t\nstruct t { x: e; }"))),
        (NewerReference, Case::Sources(sys("/// b\n@since(1.2) bits b: u8 { a = 0; }\n/// c\nconst c: b = a;"))),
        (NewerReference, Case::Sources(sys("/// b\nbits b: u8 { a = 0; @since(1.2) z = 1; }\n/// c\nconst c: b = a | z;"))),
        (NewerReference, Case::Sources(sys("/// k\n@since(1.2) capability t.k;\n/// i\ninterface i {\n/// m\nmethod m = 1 () errors(access_denied) requires t.k;\n}"))),
        (NewerReference, s("/// d\nlibrary a.b; transport syscall; version 1.2;\n/// s\nenum status: i64 { ok = 0; @since(1.2) late = -20; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () errors(late) requires none;\n}\n")),
        (SinceNotIncreasing, Case::Sources(chan(&one("method m = 1 (@since(1.2) a: u32, @since(1.1) b: u32) errors() requires none;")))),
        (SinceNotIncreasing, Case::Sources(chan(&one("method m = 1 () -> (@since(1.2) a: u32, @since(1.1) b: u32) errors() requires none;")))),
        (BadSince, Case::Sources(sys("/// e\nenum e: u8 { @since(2.0) a = 1; }"))),
        (BadSince, Case::Sources(sys("/// e\nenum e: u8 { @since(1.0) a = 1; }"))),
        (BadSince, Case::Sources(sys("/// e\nenum e: u8 { @since(1.3) a = 1; }"))),
        (BadSince, Case::Sources(sys("/// e\nenum e: u8 { @since(70000.1) a = 1; }"))),
        (BadSince, Case::Sources(sys("/// e\nenum e: u8 { @since(1.70000) a = 1; }"))),
        (BadSince, Case::Sources(chan(&one("@since(1.2) method m = 1 (@since(1.1) a: u32) errors() requires none;")))),
        (BadSince, Case::Sources(sys("/// e\n@since(1.2) enum e: u8 { @since(1.1) a = 1; }"))),
        (BadSince, Case::Sources(sys("/// e\n@since(1.2) bits e: u8 { @since(1.1) a = 1; }"))),
        (BadSince, Case::Sources(sys("/// e\n@since(1.2) sized struct e { @since(1.1) a: u8; }"))),
        (BadHandleType, Case::Sources(sys(&one("method m = 1 (h: handle<nothing>) errors(bad_handle, wrong_type) requires none;")))),
        (BadHandleType, Case::Sources(sys(&one("method m = 1 (h: handle<any, nothing>) errors(bad_handle, access_denied) requires none;")))),
        (BadHandleType, Case::Sources(sys(&one("method m = 1 (h: handle<any, read | read>) errors(bad_handle, access_denied) requires none;")))),
        (DuplicateOrdinal, Case::Sources(sys("/// i\ninterface i {\n/// m\nmethod a = 1 () errors() requires none;\n/// n\nmethod b = 1 () errors() requires none;\n}"))),
        (DuplicateOrdinal, Case::Sources(sys("/// i\ninterface i {\n/// m\nmethod a = 1 () errors() requires none;\n}\n/// j\ninterface j {\n/// n\nmethod b = 1 () errors() requires none;\n}"))),
        (BadOrdinal, Case::Sources(chan(&one("method m = 0 () errors() requires none;")))),
        (BadOrdinal, Case::Sources(chan(&one("method m = 2147483648 () errors() requires none;")))),
        (BadOrdinal, Case::Sources(sys(&one("method m = 65536 () errors() requires none;")))),
        // 0x050F: its `mov eax, imm32` would hold the two bytes of `syscall` (§10).
        (BadOrdinal, Case::Sources(sys(&one("method m = 1295 () errors() requires none;")))),
        (DuplicateParam, Case::Sources(chan(&one("method m = 1 (a: u32, a: u32) errors() requires none;")))),
        (DuplicateParam, Case::Sources(chan(&one("method m = 1 (a: u32) -> (a: u32) errors() requires none;")))),
        (MissingErrors, Case::Sources(sys(&one("method m = 1 () requires none;")))),
        (ErrorsNotAllowed, Case::Sources(chan(&one("oneway method m = 1 () errors() requires none;")))),
        (ErrorsNotAllowed, Case::Sources(sys(&one("method m = 1 () -> never errors() requires none;")))),
        (NotAStatus, Case::Sources(sys(&one("method m = 1 () errors(nothing) requires none;")))),
        (DuplicateError, Case::Sources(sys(&one("method m = 1 () errors(fault, fault) requires none;")))),
        (OkInErrors, Case::Sources(sys(&one("method m = 1 () errors(ok) requires none;")))),
        (UndeclaredDerivedError, Case::Sources(sys(&one("method m = 1 (a: u32) errors() requires none;")))),
        (UndeclaredDerivedError, Case::Sources(sys(&one("method m = 1 (h: handle<any>) errors() requires none;")))),
        (UndeclaredDerivedError, Case::Sources(sys(&one("method m = 1 (h: handle<any, read>) errors(bad_handle) requires none;")))),
        (UndeclaredDerivedError, Case::Sources(sys(&one("method m = 1 (h: handle<channel>) errors(bad_handle) requires none;")))),
        (UndeclaredDerivedError, Case::Sources(sys(&one("method m = 1 () -> (a: u32) errors() requires none;")))),
        (UndeclaredDerivedError, Case::Sources(sys(&one("method m = 1 (a: vector<u8, 3>) errors(invalid_args) requires none;")))),
        (UndeclaredDerivedError, Case::Sources(sys("/// s\nsized struct s { a: u32; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () -> (a: s) errors(fault) requires none;\n}"))),
        (UndeclaredDerivedError, Case::Sources(sys(&one("method m = 1 (@nonzero n: u64) errors() requires none;")))),
        (UnknownCapability, Case::Sources(sys(&one("method m = 1 () errors(access_denied) requires a.b;")))),
        (DuplicateCapability, Case::Sources(sys("/// c\ncapability a.b;\n/// d\ncapability a.b;"))),
        (ElseNotAllowed, Case::Sources(chan("/// c\ncapability a.b;\n/// i\ninterface i {\n/// m\nmethod m = 1 () errors(not_supported) requires a.b else not_supported;\n}"))),
        (DeniedNotDeclared, Case::Sources(sys("/// c\ncapability a.b;\n/// i\ninterface i {\n/// m\nmethod m = 1 () errors() requires a.b;\n}"))),
        (DeniedNotDeclared, Case::Sources(sys("/// c\ncapability a.b;\n/// i\ninterface i {\n/// m\nmethod m = 1 () -> never requires a.b;\n}"))),
        (OnewayNotAllowed, Case::Sources(sys(&one("oneway method m = 1 () requires none;")))),
        (OnewayNotAllowed, Case::Sources(chan(&one("oneway method m = 1 () -> (a: u32) requires none;")))),
        (NeverNotAllowed, Case::Sources(chan(&one("method m = 1 () -> never requires none;")))),
        (ReservedNotAllowed, Case::Sources(chan(&one("@reserved method m = 1 () errors(not_supported) requires none;")))),
        (ReservedNeedsNotSupported, Case::Sources(sys(&one("@reserved method m = 1 () errors() requires none;")))),
        (TooManyRegisters, Case::Sources(sys(&one("method m = 1 (a: vector<u8, 2>, b: vector<u8, 2>, c: vector<u8, 2>, d: u64) errors(invalid_args, fault) requires none;")))),
        (MessageTooLarge, Case::Sources(chan(&one("method m = 1 (a: vector<u8, 65536>) errors() requires none;")))),
        (MessageTooLarge, Case::Sources(chan(&one("method m = 1 () -> (a: vector<u8, 65536>) errors() requires none;")))),
        (TooManyHandles, Case::Sources(chan(&one("method m = 1 (a: vector<handle<any>, 65>) errors() requires none;")))),
        (TooManyHandles, Case::Sources(chan(&one("method m = 1 (a: vector<handle<any>, 64>, b: handle<any>) errors() requires none;")))),
        (UnknownAttribute, Case::Sources(sys("/// e\nenum e: u8 { @nope a = 1; }"))),
        (MisplacedAttribute, Case::Sources(sys("/// s\nstruct s { @since(1.1) a: u32; }"))),
        (MisplacedAttribute, Case::Sources(sys(&one("method m = 1 (@since(1.1) a: u64) errors() requires none;")))),
        (MisplacedAttribute, Case::Sources(chan(&one("method m = 1 () -> (@nonzero a: u32) errors() requires none;")))),
        (MisplacedAttribute, Case::Sources(chan(&one("method m = 1 () -> (@also_on(limit) a: u32) errors(limit) requires none;")))),
        (MisplacedAttribute, Case::Sources(sys("/// i\ninterface i {\n/// m\n@nonzero method m = 1 () errors() requires none;\n}"))),
        (MisplacedAttribute, Case::Sources(sys("/// i\n@since(1.1)\ninterface i {\n/// m\nmethod m = 1 () errors() requires none;\n}"))),
        (MisplacedAttribute, Case::Sources(sys(&one("method m = 1 () -> (@also_on(limit) v: vector<u8, 3>) errors(fault, limit) requires none;")))),
        (DuplicateAttribute, Case::Sources(sys("/// e\nenum e: u8 { @since(1.1) @since(1.2) a = 1; }"))),
        (BadAttributeArgs, Case::Sources(sys("/// e\nenum e: u8 { @since a = 1; }"))),
        (BadAttributeArgs, Case::Sources(sys("/// e\nenum e: u8 { @since(x) a = 1; }"))),
        (BadAttributeArgs, Case::Sources(sys(&one("method m = 1 (@nonzero(x) a: u64) errors(invalid_args) requires none;")))),
        (BadAttributeArgs, Case::Sources(sys(&one("method m = 1 () -> (@count(a) b: vector<u8, 2>) errors(fault, invalid_args) requires none;")))),
        (BadAttributeArgs, Case::Sources(sys(&one("method m = 1 () -> (@also_on() b: u64) errors(fault) requires none;")))),
        (BadAttributeArgs, Case::Sources(sys(&one("method m = 1 () -> (@also_on(1.2) b: u64) errors(fault) requires none;")))),
        (BadAttributeArgs, Case::Sources(sys(&one("method m = 1 () -> (@also_on(a.b) c: u64) errors(fault) requires none;")))),
        (BadAttributeArgs, Case::Sources(sys(&one("method m = 1 () -> (@also_on(fault, fault) b: u64) errors(fault) requires none;")))),
        (BadAttributeArgs, Case::Sources(sys("/// i\ninterface i {\n/// m\n@reserved(x) method m = 1 () errors(not_supported) requires none;\n}"))),
        (CountRequired, Case::Sources(sys(&one("method m = 1 () -> (b: vector<u8, 2>) errors(fault) requires none;")))),
        (BadCount, Case::Sources(sys(&one("method m = 1 () -> (@count(nope.field) b: vector<u8, 2>) errors(fault) requires none;")))),
        (BadCount, Case::Sources(sys("/// s\nsized struct s { n: u32; @since(1.1) m: u32; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () -> (r: s, @count(r.m) b: vector<u8, 2>) errors(fault, invalid_args) requires none;\n}"))),
        (BadCount, Case::Sources(sys("/// s\nstruct s { n: u16; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () -> (r: s, @count(r.n) b: vector<u8, 2>) errors(fault) requires none;\n}"))),
        (BadCount, Case::Sources(sys("/// s\nstruct s { n: u32; }\n/// i\ninterface i {\n/// m\nmethod m = 1 () -> (r: s, @count(r.n) b: u64) errors(fault) requires none;\n}"))),
        (AlsoOnNotDeclared, Case::Sources(sys(&one("method m = 1 () -> (@also_on(limit) b: u64) errors(fault) requires none;")))),
        (NonzeroType, Case::Sources(sys(&one("method m = 1 (@nonzero a: bool) errors(invalid_args) requires none;")))),
    ]
}

#[test]
fn every_malformed_definition_is_refused_for_the_right_reason() {
    for (index, (code, case)) in cases().iter().enumerate() {
        assert_eq!(
            run(case),
            Err(*code),
            "case {index}: wrong outcome for {code:?}: {case:?}"
        );
    }
}

#[test]
fn every_rejection_has_a_definition_that_produces_it() {
    let covered: HashSet<Code> = cases().iter().map(|(code, _)| *code).collect();
    let missing: Vec<Code> = Code::ALL
        .iter()
        .copied()
        .filter(|c| !covered.contains(c))
        .collect();
    assert!(missing.is_empty(), "no test produces: {missing:?}");
}

#[test]
fn a_struct_chain_at_the_limit_is_accepted_and_a_long_one_is_refused_quickly() {
    // Exactly the limit is fine.
    assert_eq!(compile_all(&sys(&chain_of_structs(16)), None), Ok(()));
    // A chain much longer than the limit is refused long before it could overflow a stack.
    assert_eq!(
        compile_all(&sys(&chain_of_structs(4000)), None),
        Err(Code::NestingTooDeep)
    );
}

#[test]
fn an_item_may_be_added_in_a_later_minor() {
    let source = sys(
        "/// c\n@since(1.1) const c: u32 = 1;\n/// e\n@since(1.2) enum e: u8 { a = 1; }\n/// b\n@since(1.1) bits b: u8 { x = 0; @since(1.2) y = 1; }\n/// s\n@since(1.2) struct t { x: u8; }\n/// z\n@since(1.1) sized struct z { @since(1.1) x: u8; }\n/// k\n@since(1.1) capability k.m;",
    );
    assert_eq!(compile_all(&source, None), Ok(()));
}

#[test]
fn a_reference_to_a_newer_item_is_a_recorded_break_at_major_0_and_fine_within_one_minor() {
    // At major 0 nothing is promised (§9): the fingerprint gate reports the change instead.
    let major_0 = "/// d\nlibrary a.b; transport syscall; version 0.2;\n/// s\nenum status: i64 { ok = 0; invalid_args = -1; }\n/// e\n@since(0.2) enum e: u8 { a = 1; }\n/// i\ninterface i {\n/// m\nmethod m = 1 (x: e) errors(invalid_args) requires none;\n}\n";
    assert_eq!(run(&s(major_0)), Ok(()));
    // From major 1, an item added in the same minor as what it names is fine.
    let same_minor = sys(
        "/// e\n@since(1.2) enum e: u8 { a = 1; }\n/// t\n@since(1.2) struct t { x: e; }\n/// i\ninterface i {\n/// m\n@since(1.2) method m = 1 (x: e) errors(invalid_args) requires none;\n}",
    );
    assert_eq!(compile_all(&same_minor, None), Ok(()));
}

#[test]
fn doc_text_is_trimmed_so_it_cannot_become_an_indented_code_block() {
    let input = Input {
        file: "d".to_string(),
        text: b"///       indented\nlibrary a.b; transport syscall; version 1.0;\n".to_vec(),
    };
    let comp = compile(&[input], None).expect("valid");
    assert_eq!(comp.libraries[0].doc, ["indented".to_string()]);
}

#[test]
fn a_diagnostic_says_where_the_problem_is() {
    let source = "/// d\nlibrary a.b; transport syscall; version 1.0;\n/// c\nconst c: u8 = 256;\n";
    let input = Input {
        file: "f.oidl".to_string(),
        text: source.as_bytes().to_vec(),
    };
    let error = compile(&[input], None).expect_err("out of range");
    assert_eq!(error.code, Code::ConstOutOfRange);
    assert_eq!(error.file, "f.oidl");
    assert_eq!(error.line, 4);
    assert_eq!(error.column, 15);
    let shown = error.to_string();
    assert!(shown.starts_with("f.oidl:4:15:"), "{shown}");
}

const KERNEL: &str = include_str!("../../../../sdk/interfaces/origin.kernel.oidl");
const DIRECTORY: &str = include_str!("../../../../sdk/interfaces/origin.directory.oidl");
const ECHO: &str = include_str!("../../tests/fixtures/echo.oidl");
const CALLS: &str = include_str!("../../tests/fixtures/calls.oidl");

/// The versions file the kernel's definition names its version from (§9): the real one, the
/// one place each number is written (§62).
const VERSIONS: &str = include_str!("../../../../origin-version.toml");

fn good_inputs() -> Vec<Input> {
    [
        ("origin.kernel.oidl", KERNEL),
        ("origin.directory.oidl", DIRECTORY),
        ("echo.oidl", ECHO),
        ("calls.oidl", CALLS),
    ]
    .iter()
    .map(|(file, text)| Input {
        file: file.to_string(),
        text: text.as_bytes().to_vec(),
    })
    .collect()
}

#[test]
fn the_shipped_definitions_compile() {
    let comp = compile(&good_inputs(), Some(VERSIONS)).expect("valid");
    assert_eq!(comp.libraries.len(), 4);
    assert!(comp.status.is_some());
    assert!(comp.rights.is_some());
    assert!(comp.object_type.is_some());
    assert_eq!(comp.max_message_bytes, Some(65536));
    assert_eq!(comp.max_message_handles, Some(64));
    // The kernel library is built first even though it is not given first.
    let names: Vec<&str> = comp.libraries.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names[0], "origin.kernel");
}

#[test]
fn layouts_hold_the_arithmetic_invariants_of_the_specification() {
    let comp = compile(&good_inputs(), Some(VERSIONS)).expect("valid");
    for library in &comp.libraries {
        for structure in &library.structs {
            let mut regions: Vec<(u64, u64)> = structure
                .fields
                .iter()
                .map(|f| (u64::from(f.offset), layout::fixed_layout(&f.ty).0))
                .chain(
                    structure
                        .padding
                        .iter()
                        .map(|(a, l)| (u64::from(*a), u64::from(*l))),
                )
                .collect();
            if structure.sized {
                regions.push((0, 4));
            }
            regions.sort_unstable();
            let mut at = 0;
            for (offset, length) in regions {
                assert_eq!(offset, at, "{}: gap or overlap", structure.name);
                at += length;
            }
            assert_eq!(at, u64::from(structure.size), "{}", structure.name);
            assert_eq!(structure.size % structure.align, 0, "{}", structure.name);
        }
        for interface in &library.interfaces {
            for method in &interface.methods {
                for params in [&method.params, &method.returns] {
                    let mut last_inline = 0;
                    for minor in method.minors(library.version.minor) {
                        let layout = layout::message_layout(params, minor);
                        let mut regions: Vec<(u64, u64)> = layout
                            .placed
                            .iter()
                            .map(|p| (p.offset, 0))
                            .chain(layout.padding.iter().copied())
                            .collect();
                        regions.sort_unstable();
                        assert_eq!(layout.inline_size & 7, 0);
                        assert!(layout.inline_size >= last_inline, "{}", method.name);
                        last_inline = layout.inline_size;
                    }
                }
            }
        }
    }
}

#[test]
fn a_method_may_name_a_later_version_and_a_version_key_is_read() {
    let comp = compile(&good_inputs(), Some(VERSIONS)).expect("valid");
    let kernel = comp
        .libraries
        .iter()
        .find(|l| l.name == "origin.kernel")
        .expect("kernel");
    assert_eq!(
        (kernel.version.major, kernel.version.minor),
        origin_abi::ABI_VERSION,
        "read from origin-version.toml"
    );
    assert_eq!(kernel.version_key.as_deref(), Some("user-abi"));
    let echo = comp
        .libraries
        .iter()
        .find(|l| l.name == "origin.fixture.echo")
        .expect("echo");
    assert_eq!((echo.version.major, echo.version.minor), (1, 2));
    assert_eq!(echo.version_key, None);
}

/// The one `syscall` number below the limit whose load into EAX would hold the two bytes of
/// `syscall` is 1295, and a `channel` ordinal of that value is no load into anything.
#[test]
fn a_syscall_number_whose_load_would_hold_a_syscall_is_the_one_refused() {
    use crate::sema::{MAX_SYSCALL_NUMBER, number_holds_syscall};
    let refused: Vec<u32> = (0..=MAX_SYSCALL_NUMBER as u32)
        .filter(|&number| number_holds_syscall(number))
        .collect();
    assert_eq!(refused, [0x050F]);
    // Above the limit the pair can sit in the higher bytes too: the rule is on the bytes.
    assert!(number_holds_syscall(0x0005_0F00));
    assert!(number_holds_syscall(0x050F_0000));
    assert!(!number_holds_syscall(0x0F05));
    assert!(!number_holds_syscall(0x0F00_0005));
    // The definition is refused with the reason, and its neighbours are taken.
    let inputs = |sources: Vec<(&'static str, String)>| -> Vec<Input> {
        sources
            .into_iter()
            .map(|(file, text)| Input {
                file: file.to_string(),
                text: text.into_bytes(),
            })
            .collect()
    };
    let text = |number: u32| {
        sys(&one(&format!(
            "method m = {number} () errors() requires none;"
        )))
    };
    let problem = compile(&inputs(text(1295)), None).expect_err("1295 is refused");
    assert_eq!(problem.code, Code::BadOrdinal);
    assert!(problem.message.contains("0F 05"), "{}", problem.message);
    for number in [1294, 1296] {
        assert!(compile(&inputs(text(number)), None).is_ok(), "{number}");
    }
    // A channel ordinal is not loaded into a register by any stub.
    assert!(
        compile(
            &inputs(chan(&one("method m = 1295 () errors() requires none;"))),
            None
        )
        .is_ok()
    );
}

/// Every handle a type can carry, walking into structs, arrays and vectors.
fn handles_in(ty: &crate::model::Type, found: &mut Vec<crate::model::HandleType>) {
    use crate::model::Type;
    match ty {
        Type::Handle(handle) => found.push(handle.clone()),
        Type::Struct(structure) => {
            for field in &structure.fields {
                handles_in(&field.ty, found);
            }
        }
        Type::Array(inner, _) | Type::Vector(inner, _) => handles_in(inner, found),
        Type::Bool | Type::Int(_) | Type::Enum(_) | Type::Bits(_) | Type::String(_) => {}
    }
}

/// Row O2 of the system-call threat model is closed for step 4 by absence (decision item 206;
/// ADR-0005 Amendment 8, G2): no call of `origin.kernel` creates a thread or a process. A call
/// that hands back a handle to a process or a thread, anywhere in its results, would reopen it,
/// and this test fails then. So does any other result that is a handle of any type, which could
/// name a new process or thread, besides `handle_duplicate`'s, which names an object its caller
/// already holds a handle to.
#[test]
fn no_kernel_call_returns_a_new_process_or_thread() {
    let comp = compile(&good_inputs(), Some(VERSIONS)).expect("valid");
    let kernel = comp
        .libraries
        .iter()
        .find(|library| library.name == "origin.kernel")
        .expect("origin.kernel");
    let mut methods = 0;
    for interface in &kernel.interfaces {
        for method in &interface.methods {
            methods += 1;
            let mut found = Vec::new();
            for result in &method.returns {
                handles_in(&result.ty, &mut found);
            }
            for handle in found {
                let object = handle.object.as_deref();
                assert!(
                    object != Some("process") && object != Some("thread"),
                    "{} returns a {:?} handle: row O2 reopens (ADR-0005 G2)",
                    method.name,
                    object
                );
                assert!(
                    object.is_some() || method.name == "handle_duplicate",
                    "{} returns a handle of any type, which could be a new process or thread",
                    method.name
                );
            }
        }
    }
    // The definition the threat model's row cites: its twenty calls at `user-abi` 0.7.
    assert_eq!(methods, 20);
    assert_eq!((kernel.version.major, kernel.version.minor), (0, 7));
}
