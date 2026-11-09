//! The C header generator (`docs/spec/OIDL.md` §11). A C compiler checks every layout against
//! the specification's arithmetic: each struct has `_Static_assert`s on its size and offsets, and
//! padding is named so that `{0}` in C writes it (§5.2).

use super::{Generated, Text};
use crate::layout;
use crate::model::{Compilation, Int, Library, Method, Param, Struct, Transport, Type};
use crate::names::{module, shouty};

fn int_literal(int: Int, value: i128) -> String {
    if int == Int::I64 && value == i128::from(i64::MIN) {
        return "(INT64_C(-9223372036854775807) - 1)".to_string();
    }
    let suffix = match int {
        Int::U8 => "UINT8_C",
        Int::U16 => "UINT16_C",
        Int::U32 => "UINT32_C",
        Int::U64 => "UINT64_C",
        Int::I8 => "INT8_C",
        Int::I16 => "INT16_C",
        Int::I32 => "INT32_C",
        Int::I64 => "INT64_C",
    };
    format!("{suffix}({value})")
}

fn type_name(library: &str, name: &str) -> String {
    format!("{}_{name}_t", module(library))
}

fn fixed_c(ty: &Type) -> String {
    match ty {
        Type::Bool => "uint8_t".to_string(),
        Type::Int(int) => int.c().to_string(),
        Type::Enum(e) => type_name(&e.library, &e.name),
        Type::Bits(b) => type_name(&b.library, &b.name),
        Type::Struct(s) => type_name(&s.library, &s.name),
        Type::Handle(_) => "origin_handle_t".to_string(),
        Type::Array(..) | Type::Vector(..) | Type::String(_) => "uint8_t".to_string(),
    }
}

fn element_c(ty: &Type) -> String {
    match ty {
        Type::Array(elem, _) | Type::Vector(elem, _) => fixed_c(elem),
        _ => "uint8_t".to_string(),
    }
}

/// A struct member: `T name[3][2];` for nested arrays.
fn c_field(name: &str, ty: &Type) -> String {
    let mut dims = String::new();
    let mut inner = ty;
    while let Type::Array(elem, count) = inner {
        dims.push_str(&format!("[{count}]"));
        inner = elem;
    }
    format!("{} {name}{dims};", fixed_c(inner))
}

fn gen_struct(t: &mut Text, s: &Struct) {
    let name = type_name(&s.library, &s.name);
    let tag = format!("{}_{}", module(&s.library), s.name);
    t.c_doc(&s.doc);
    t.open(format!("typedef struct {tag} {{"));
    if s.sized {
        t.line("uint32_t size; /* set by the caller */");
    }
    // Fields and padding, in offset order.
    let mut items: Vec<(u32, String)> = s
        .fields
        .iter()
        .map(|f| {
            let line = c_field(&f.name, &f.ty);
            (f.offset, line)
        })
        .collect();
    items.extend(
        s.padding
            .iter()
            .map(|(at, len)| (*at, format!("uint8_t oidl_pad_{at}[{len}];"))),
    );
    items.sort_by_key(|(at, _)| *at);
    for (_, line) in items {
        t.line(line);
    }
    t.shut(format!("}} {name};"));
    t.line(format!(
        "ORIGIN_OIDL_ASSERT(sizeof({name}) == {}, \"{}.{} size\");",
        s.size, s.library, s.name
    ));
    for field in &s.fields {
        t.line(format!(
            "ORIGIN_OIDL_ASSERT(offsetof({name}, {}) == {}, \"{}.{}.{} offset\");",
            field.name, field.offset, s.library, s.name, field.name
        ));
    }
    if s.sized {
        t.line(format!(
            "ORIGIN_OIDL_ASSERT(offsetof({name}, size) == 0, \"{}.{}.size offset\");",
            s.library, s.name
        ));
    }
    t.blank();
}

/// The C parameters of a `syscall` entry point.
fn c_params(method: &Method) -> Vec<String> {
    let mut list = Vec::new();
    for param in &method.params {
        list.extend(c_param(param, false));
    }
    for result in &method.returns {
        list.extend(c_param(result, true));
    }
    if list.is_empty() {
        list.push("void".to_string());
    }
    list
}

fn c_param(param: &Param, is_result: bool) -> Vec<String> {
    let n = &param.name;
    match &param.ty {
        Type::Vector(elem, _) => {
            let e = fixed_c(elem);
            if is_result {
                vec![format!("{e} *{n}"), format!("uint64_t {n}_capacity")]
            } else {
                vec![format!("const {e} *{n}"), format!("uint64_t {n}_count")]
            }
        }
        Type::String(_) => {
            if is_result {
                vec![format!("uint8_t *{n}"), format!("uint64_t {n}_capacity")]
            } else {
                vec![format!("const uint8_t *{n}"), format!("uint64_t {n}_count")]
            }
        }
        Type::Struct(_) => {
            let ty = fixed_c(&param.ty);
            if is_result {
                vec![format!("{ty} *{n}")]
            } else {
                vec![format!("const {ty} *{n}")]
            }
        }
        Type::Array(..) => {
            let e = element_c(&param.ty);
            if is_result {
                vec![format!("{e} *{n}")]
            } else {
                vec![format!("const {e} *{n}")]
            }
        }
        scalar => {
            let ty = fixed_c(scalar);
            if is_result {
                vec![format!("{ty} *{n}")]
            } else {
                vec![format!("{ty} {n}")]
            }
        }
    }
}

fn gen_library(lib: &Library) -> String {
    let prefix = module(&lib.name);
    let upper = shouty(&prefix);
    let mut t = Text::default();
    t.line("/* @generated by oidl. Do not edit. docs/spec/OIDL.md */");
    t.line(format!("#ifndef {upper}_H"));
    t.line(format!("#define {upper}_H"));
    t.blank();
    t.line("#include <stddef.h>");
    t.line("#include <stdint.h>");
    for used in &lib.uses {
        t.line(format!("#include <{}.h>", used.replace('.', "/")));
    }
    t.blank();
    t.line("#ifdef __cplusplus");
    t.line("extern \"C\" {");
    t.line("#endif");
    t.blank();
    t.line("#ifndef ORIGIN_OIDL_ASSERT");
    t.line("#ifdef __cplusplus");
    t.line("#define ORIGIN_OIDL_ASSERT(condition, message) static_assert(condition, message)");
    t.line("#define ORIGIN_OIDL_NORETURN [[noreturn]]");
    t.line("#else");
    t.line("#define ORIGIN_OIDL_ASSERT(condition, message) _Static_assert(condition, message)");
    t.line("#define ORIGIN_OIDL_NORETURN _Noreturn");
    t.line("#endif");
    t.line("#endif");
    t.blank();
    t.line("#ifndef ORIGIN_OIDL_HANDLE_T");
    t.line("#define ORIGIN_OIDL_HANDLE_T");
    t.line("/** A handle: a 32-bit name for a kernel object. 0 is never valid. */");
    t.line("typedef uint32_t origin_handle_t;");
    t.line("#define ORIGIN_INVALID_HANDLE ((origin_handle_t)0)");
    t.line("#endif");
    t.blank();
    for c in &lib.consts {
        t.c_doc(&c.doc);
        match &c.kind {
            crate::model::ConstKind::Int(int) => {
                t.line(format!(
                    "#define {upper}_{} {}",
                    shouty(&c.name),
                    int_literal(*int, c.value)
                ));
            }
            crate::model::ConstKind::Bits(bits) => {
                t.line(format!(
                    "#define {upper}_{} {}",
                    shouty(&c.name),
                    int_literal(bits.repr, c.value)
                ));
            }
        }
        t.blank();
    }
    for e in &lib.enums {
        t.c_doc(&e.doc);
        t.line(format!(
            "typedef {} {};",
            e.repr.c(),
            type_name(&e.library, &e.name)
        ));
        let enum_prefix = format!("{upper}_{}", shouty(&e.name));
        for member in &e.members {
            t.c_doc(&member.doc);
            t.line(format!(
                "#define {enum_prefix}_{} {}",
                shouty(&member.name),
                int_literal(e.repr, member.value)
            ));
        }
        t.blank();
    }
    for b in &lib.bits {
        t.c_doc(&b.doc);
        t.line(format!(
            "typedef {} {};",
            b.repr.c(),
            type_name(&b.library, &b.name)
        ));
        let bits_prefix = format!("{upper}_{}", shouty(&b.name));
        let mut all: u64 = 0;
        for member in &b.members {
            let mask = 1u64 << member.value;
            all |= mask;
            t.c_doc(&member.doc);
            t.line(format!(
                "#define {bits_prefix}_{} {}",
                shouty(&member.name),
                int_literal(b.repr, i128::from(mask))
            ));
        }
        t.line(format!(
            "#define {bits_prefix}_ALL {}",
            int_literal(b.repr, i128::from(all))
        ));
        t.blank();
    }
    for s in &lib.structs {
        gen_struct(&mut t, s);
        t.line(format!(
            "#define {upper}_{}_SIZE UINT32_C({})",
            shouty(&s.name),
            s.size
        ));
        t.blank();
    }
    for interface in &lib.interfaces {
        t.c_doc(&interface.doc);
        match lib.transport {
            Transport::Syscall => {
                for method in &interface.methods {
                    t.c_doc(&method.doc);
                    let params = c_params(method).join(", ");
                    if method.never {
                        t.line(format!(
                            "ORIGIN_OIDL_NORETURN void {prefix}_{}({params});",
                            method.name
                        ));
                    } else {
                        t.line(format!("int64_t {prefix}_{}({params});", method.name));
                    }
                }
            }
            Transport::Channel => {
                let newest = lib.version.minor;
                let iface = shouty(&interface.name);
                for method in &interface.methods {
                    t.c_doc(&method.doc);
                    let name = shouty(&method.name);
                    t.line(format!(
                        "#define {upper}_{iface}_{name}_ORDINAL UINT32_C({})",
                        method.ordinal
                    ));
                    t.line(format!(
                        "#define {upper}_{iface}_{name}_REQUEST_MAX_SIZE UINT32_C({})",
                        layout::max_message_size(&method.params, newest)
                    ));
                    if !method.oneway {
                        t.line(format!(
                            "#define {upper}_{iface}_{name}_RESPONSE_MAX_SIZE UINT32_C({})",
                            layout::max_message_size(&method.returns, newest)
                        ));
                    }
                }
            }
        }
        t.blank();
    }
    t.line("#ifdef __cplusplus");
    t.line("}");
    t.line("#endif");
    t.blank();
    t.line(format!("#endif /* {upper}_H */"));
    t.finish()
}

/// One header per library.
pub fn generate(comp: &Compilation) -> Vec<Generated> {
    comp.libraries
        .iter()
        .map(|lib| Generated {
            path: format!("c/{}.h", lib.name.replace('.', "/")),
            text: gen_library(lib),
        })
        .collect()
}
