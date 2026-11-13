//! Semantic analysis: resolves names, computes layouts and applies every rule of
//! `docs/spec/OIDL.md` to the parsed libraries. Everything downstream relies on what passes
//! here, so a rule that is not enforced here does not exist.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::{
    Attr, AttrArg, Bound, ConstDecl, ConstValue, Dotted, FieldDecl, InterfaceDecl, Item,
    LibraryFile, MethodDecl, Name, ParamDecl, Pos, Requires, Returns, StructDecl, TypeExpr,
    ValuesDecl, VersionDecl,
};
use crate::diag::{Code, Diagnostic};
use crate::layout::{self, is_fixed};
use crate::model::{
    Bits, Capability, Compilation, Const, ConstKind, Enum, Field, HandleType, Int, Interface,
    Library, Member, Method, Param, Requirement, StatusRef, Struct, Transport, Type, Version,
};
use crate::names::{self, pascal, shouty};
use crate::parse;

/// Most bytes in a struct or array (the message limit, ADR-0005 P8).
pub const MAX_FIXED: u64 = 65_536;
/// Registers a system call can use (ADR-0005 P4).
pub const MAX_REGISTERS: u32 = 6;
/// Deepest nesting of structs inside structs (§2): every walker over a struct recurses, so this
/// is what bounds them, and what bounds a recursive drop of the model.
pub const MAX_STRUCT_DEPTH: u32 = 16;
/// Largest `channel` ordinal (§7.1).
pub const MAX_CHANNEL_ORDINAL: u64 = 0x7FFF_FFFF;
/// Largest `syscall` number accepted in a definition.
pub const MAX_SYSCALL_NUMBER: u64 = 0xFFFF;

/// Whether loading `number` into EAX would put the two bytes of a `syscall` instruction,
/// `0F 05`, into the stub: any two adjacent bytes of the number as the 32-bit immediate of
/// `mov eax, imm32`, which is little-endian. Below [`MAX_SYSCALL_NUMBER`] that is 1295
/// (`0x050F`) alone.
///
/// `origin.core` may hold those two bytes once in each stub, at its `syscall`, and nowhere
/// else at any alignment (ADR-0005 B2; §10): the kernel accepts each call number only from
/// the address of its own, and a second pair inside a stub would be a `syscall` a jump could
/// reach with that stub's address. The build has always refused such a stub; the number is
/// refused here, where the definition is read, so that the reason is given where it is made.
#[must_use]
pub fn number_holds_syscall(number: u32) -> bool {
    number
        .to_le_bytes()
        .windows(2)
        .any(|pair| pair == [0x0F, 0x05])
}

/// One input file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// A name for messages.
    pub file: String,
    /// Its bytes.
    pub text: Vec<u8>,
}

fn fail<T>(file: &str, pos: Pos, code: Code, message: impl Into<String>) -> Result<T, Diagnostic> {
    Err(Diagnostic {
        code,
        file: file.to_string(),
        line: pos.line,
        column: pos.column,
        message: message.into(),
    })
}

fn parse_version(text: &str) -> Option<Version> {
    let (major, minor) = text.split_once('.')?;
    let parse_part = |s: &str| {
        if s.is_empty()
            || (s.len() > 1 && s.starts_with('0'))
            || !s.bytes().all(|b| b.is_ascii_digit())
        {
            None
        } else {
            s.parse::<u16>().ok()
        }
    };
    Some(Version {
        major: parse_part(major)?,
        minor: parse_part(minor)?,
    })
}

/// The `[compatibility]` keys of `origin-version.toml`.
fn compatibility_keys(text: &str) -> HashMap<String, String> {
    let mut keys = HashMap::new();
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == "[compatibility]";
            continue;
        }
        if !in_section || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let value = value.trim();
            if let Some(value) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
                keys.insert(key.trim().to_string(), value.to_string());
            }
        }
    }
    keys
}

/// What has been resolved for one library.
#[derive(Default)]
struct Symbols {
    consts: HashMap<String, Const>,
    enums: HashMap<String, Rc<Enum>>,
    bits: HashMap<String, Rc<Bits>>,
    structs: HashMap<String, Rc<Struct>>,
    interfaces: HashSet<String>,
}

struct Scope<'a> {
    file: &'a str,
    name: &'a str,
    transport: Transport,
    version: Version,
    symbols: &'a mut Symbols,
    uses: &'a HashMap<String, usize>,
    used: &'a [Option<Symbols>],
    status: Option<Rc<Enum>>,
    rights: Option<Rc<Bits>>,
    object_type: Option<Rc<Enum>>,
    limits: (Option<u64>, Option<u64>),
    capabilities: Vec<Capability>,
    syscall_numbers: HashSet<u32>,
}

impl Scope<'_> {
    /// The symbols of the library this name is qualified with, or this library's.
    fn library_of(&self, name: &Name) -> Result<Option<&Symbols>, Diagnostic> {
        match &name.qualifier {
            None => Ok(None),
            Some(qualifier) => match self.uses.get(qualifier) {
                Some(index) => Ok(self.used.get(*index).and_then(Option::as_ref)),
                None => fail(self.file, name.pos, Code::UnknownName, qualifier.clone()),
            },
        }
    }

    fn resolve_named(&self, name: &Name) -> Result<Type, Diagnostic> {
        if name.qualifier.is_none()
            && let Some(int) = Int::from_name(&name.item)
        {
            return Ok(Type::Int(int));
        }
        let symbols = match self.library_of(name)? {
            Some(symbols) => symbols,
            None => &*self.symbols,
        };
        if let Some(e) = symbols.enums.get(&name.item) {
            return Ok(Type::Enum(e.clone()));
        }
        if let Some(b) = symbols.bits.get(&name.item) {
            return Ok(Type::Bits(b.clone()));
        }
        if let Some(s) = symbols.structs.get(&name.item) {
            return Ok(Type::Struct(s.clone()));
        }
        if symbols.consts.contains_key(&name.item) || symbols.interfaces.contains(&name.item) {
            return fail(self.file, name.pos, Code::WrongKind, name.item.clone());
        }
        fail(self.file, name.pos, Code::UnknownName, name.item.clone())
    }

    fn resolve_bound(&self, bound: &Bound, max: u64, what: &str) -> Result<u32, Diagnostic> {
        let (value, pos) = match bound {
            Bound::Number(value, pos) => (*value, *pos),
            Bound::Name(name) => {
                let symbols = match self.library_of(name)? {
                    Some(symbols) => symbols,
                    None => &*self.symbols,
                };
                let Some(found) = symbols.consts.get(&name.item) else {
                    if symbols.enums.contains_key(&name.item)
                        || symbols.bits.contains_key(&name.item)
                        || symbols.structs.contains_key(&name.item)
                        || symbols.interfaces.contains(&name.item)
                    {
                        return fail(self.file, name.pos, Code::WrongKind, name.item.clone());
                    }
                    return fail(self.file, name.pos, Code::UnknownName, name.item.clone());
                };
                let unsigned = matches!(found.kind, ConstKind::Int(int) if !int.signed());
                if !unsigned {
                    return fail(self.file, name.pos, Code::BoundOutOfRange, what);
                }
                (u64::try_from(found.value).unwrap_or(u64::MAX), name.pos)
            }
        };
        if value == 0 || value > max {
            return fail(self.file, pos, Code::BoundOutOfRange, what);
        }
        u32::try_from(value).or(fail(self.file, pos, Code::BoundOutOfRange, what))
    }

    fn resolve_handle(
        &self,
        object: &crate::ast::Ident,
        rights: &[crate::ast::Ident],
        optional: bool,
        pos: Pos,
    ) -> Result<HandleType, Diagnostic> {
        let Some(object_type) = &self.object_type else {
            return fail(
                self.file,
                pos,
                Code::MissingRole,
                "a handle type needs object_type",
            );
        };
        let Some(rights_bits) = &self.rights else {
            return fail(
                self.file,
                pos,
                Code::MissingRole,
                "a handle type needs rights",
            );
        };
        let (object_name, object_value) = if object.text == "any" {
            (None, 0)
        } else {
            match object_type.members.iter().find(|m| m.name == object.text) {
                Some(m) => (Some(m.name.clone()), u64::try_from(m.value).unwrap_or(0)),
                None => {
                    return fail(
                        self.file,
                        object.pos,
                        Code::BadHandleType,
                        format!("no object type {}", object.text),
                    );
                }
            }
        };
        let mut names = Vec::new();
        let mut mask = 0u64;
        for right in rights {
            let Some(member) = rights_bits.members.iter().find(|m| m.name == right.text) else {
                return fail(
                    self.file,
                    right.pos,
                    Code::BadHandleType,
                    format!("no right {}", right.text),
                );
            };
            if names.contains(&member.name) {
                return fail(
                    self.file,
                    right.pos,
                    Code::BadHandleType,
                    format!("the right {} is listed twice", right.text),
                );
            }
            names.push(member.name.clone());
            mask |= 1u64 << member.value;
        }
        names.sort();
        Ok(HandleType {
            object: object_name,
            object_value,
            rights: names,
            rights_mask: mask,
            optional,
        })
    }

    fn resolve_type(&self, expr: &TypeExpr) -> Result<Type, Diagnostic> {
        match expr {
            TypeExpr::Bool(_) => Ok(Type::Bool),
            TypeExpr::Named(name) => self.resolve_named(name),
            TypeExpr::Handle {
                object,
                rights,
                optional,
                pos,
            } => Ok(Type::Handle(
                self.resolve_handle(object, rights, *optional, *pos)?,
            )),
            TypeExpr::Array(inner, bound, pos) => {
                let elem = self.resolve_type(inner)?;
                let count = self.resolve_bound(bound, MAX_FIXED, "array length")?;
                if layout::is_fixed(&elem) {
                    let (size, _) = layout::fixed_layout(&elem);
                    if size.saturating_mul(u64::from(count)) > MAX_FIXED {
                        return fail(self.file, *pos, Code::TooLarge, "an array is too large");
                    }
                }
                Ok(Type::Array(Box::new(elem), count))
            }
            TypeExpr::Vector(inner, bound, pos) => {
                let elem = self.resolve_type(inner)?;
                let ok = is_fixed(&elem) || matches!(&elem, Type::Handle(h) if !h.optional);
                if !ok {
                    return fail(
                        self.file,
                        *pos,
                        Code::BadPosition,
                        "a vector element must be fixed-size or a handle",
                    );
                }
                let count = self.resolve_bound(bound, u64::from(u32::MAX), "vector bound")?;
                Ok(Type::Vector(Box::new(elem), count))
            }
            TypeExpr::String(bound, _) => Ok(Type::String(self.resolve_bound(
                bound,
                u64::from(u32::MAX),
                "string bound",
            )?)),
        }
    }

    /// From major 1, an item may not name an item of its own library that a later minor added
    /// (§9): no version of the definition has both, so the reference cannot be kept
    /// compatibly. `named` is when the named item was added, `than` when the naming one was.
    /// At major 0 such a reference is a break the fingerprint gate reports, not a refusal.
    fn refuse_newer(
        &self,
        pos: Pos,
        library: &str,
        named: Version,
        than: Version,
        what: &str,
    ) -> Result<(), Diagnostic> {
        if self.version.major >= 1 && library == self.name && named > than {
            return fail(
                self.file,
                pos,
                Code::NewerReference,
                format!("{what} is from {named}, which is newer than {than}"),
            );
        }
        Ok(())
    }

    /// As [`Scope::refuse_newer`], for the item a type names, if it names one.
    fn refuse_newer_type(&self, pos: Pos, ty: &Type, than: Version) -> Result<(), Diagnostic> {
        match ty {
            Type::Enum(e) => self.refuse_newer(pos, &e.library, e.since, than, &e.name),
            Type::Bits(b) => self.refuse_newer(pos, &b.library, b.since, than, &b.name),
            Type::Struct(s) => self.refuse_newer(pos, &s.library, s.since, than, &s.name),
            Type::Array(elem, _) | Type::Vector(elem, _) => self.refuse_newer_type(pos, elem, than),
            Type::Bool | Type::Int(_) | Type::String(_) | Type::Handle(_) => Ok(()),
        }
    }

    /// As [`Scope::refuse_newer`], for a status the item names, through this library's own
    /// status enumeration.
    fn refuse_newer_status(
        &self,
        name: &crate::ast::Ident,
        than: Version,
    ) -> Result<(), Diagnostic> {
        let Some(status) = &self.status else {
            return Ok(());
        };
        match status.members.iter().find(|m| m.name == name.text) {
            Some(member) => {
                self.refuse_newer(name.pos, &status.library, member.since, than, &member.name)
            }
            None => Ok(()),
        }
    }

    fn status_member(&self, name: &crate::ast::Ident) -> Result<StatusRef, Diagnostic> {
        let Some(status) = &self.status else {
            return fail(
                self.file,
                name.pos,
                Code::MissingRole,
                "errors need the status enumeration",
            );
        };
        match status.members.iter().find(|m| m.name == name.text) {
            Some(member) if member.name == "ok" => {
                fail(self.file, name.pos, Code::OkInErrors, "ok is not an error")
            }
            Some(member) => Ok(StatusRef {
                name: member.name.clone(),
                value: i64::try_from(member.value).unwrap_or(i64::MIN),
            }),
            None => fail(self.file, name.pos, Code::NotAStatus, name.text.clone()),
        }
    }
}

const KNOWN_ATTRS: &[&str] = &["since", "nonzero", "count", "also_on", "reserved"];

/// Checks that every attribute is known, allowed here and not repeated.
fn check_attrs(file: &str, attrs: &[Attr], allowed: &[&str]) -> Result<(), Diagnostic> {
    let mut seen = HashSet::new();
    for attr in attrs {
        if !KNOWN_ATTRS.contains(&attr.name.as_str()) {
            return fail(file, attr.pos, Code::UnknownAttribute, attr.name.clone());
        }
        if !allowed.contains(&attr.name.as_str()) {
            return fail(file, attr.pos, Code::MisplacedAttribute, attr.name.clone());
        }
        if !seen.insert(attr.name.as_str()) {
            return fail(file, attr.pos, Code::DuplicateAttribute, attr.name.clone());
        }
    }
    Ok(())
}

fn find_attr<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a Attr> {
    attrs.iter().find(|a| a.name == name)
}

/// The version `@since` gives, or `default`.
fn since_of(
    file: &str,
    attrs: &[Attr],
    library: Version,
    default: Version,
) -> Result<Version, Diagnostic> {
    let Some(attr) = find_attr(attrs, "since") else {
        return Ok(default);
    };
    let [AttrArg::Version(major, minor)] = attr.args.as_slice() else {
        return fail(
            file,
            attr.pos,
            Code::BadAttributeArgs,
            "@since(major.minor)",
        );
    };
    let major = u16::try_from(*major).or(fail(file, attr.pos, Code::BadSince, "major"))?;
    let minor = u16::try_from(*minor).or(fail(file, attr.pos, Code::BadSince, "minor"))?;
    if major != library.major || minor == 0 || minor > library.minor {
        return fail(
            file,
            attr.pos,
            Code::BadSince,
            format!(
                "@since({major}.{minor}) in a library at version {library}: it must name this \
                 major and a minor from 1 to the current one"
            ),
        );
    }
    Ok(Version { major, minor })
}

fn no_args(file: &str, attr: &Attr) -> Result<(), Diagnostic> {
    if attr.args.is_empty() {
        Ok(())
    } else {
        fail(file, attr.pos, Code::BadAttributeArgs, attr.name.clone())
    }
}

/// The version of an item with no `@since`.
const fn item_base(library: Version) -> Version {
    Version {
        major: library.major,
        minor: 0,
    }
}

/// A member's or field's `@since`, which may not be older than the item that holds it.
fn member_since(
    scope: &Scope<'_>,
    attrs: &[Attr],
    pos: Pos,
    item: Version,
) -> Result<Version, Diagnostic> {
    let since = since_of(scope.file, attrs, scope.version, item)?;
    if since < item {
        return fail(
            scope.file,
            pos,
            Code::BadSince,
            "older than the item that holds it",
        );
    }
    Ok(since)
}

/// How deeply structs nest in `ty`.
fn struct_depth(ty: &Type) -> u32 {
    match ty {
        Type::Struct(s) => s.depth,
        Type::Array(elem, _) | Type::Vector(elem, _) => struct_depth(elem),
        _ => 0,
    }
}

fn fits(int: Int, value: i128) -> bool {
    value >= int.min() && value <= int.max()
}

fn member_value(negative: bool, magnitude: u64) -> i128 {
    let value = i128::from(magnitude);
    if negative {
        value.saturating_neg()
    } else {
        value
    }
}

/// Builds an `enum`'s members.
fn enum_decl(scope: &Scope<'_>, decl: &ValuesDecl) -> Result<Enum, Diagnostic> {
    let file = scope.file;
    let repr = match (&decl.repr.qualifier, Int::from_name(&decl.repr.item)) {
        (None, Some(int)) => int,
        _ => {
            return fail(
                file,
                decl.repr.pos,
                Code::BadUnderlying,
                "an enum needs an integer type",
            );
        }
    };
    if decl.members.is_empty() {
        return fail(
            file,
            decl.name.pos,
            Code::EmptyDeclaration,
            decl.name.text.clone(),
        );
    }
    check_attrs(file, &decl.attrs, &["since"])?;
    let item_since = since_of(file, &decl.attrs, scope.version, item_base(scope.version))?;
    let mut names = HashSet::new();
    let mut values = HashSet::new();
    let mut members = Vec::new();
    for member in &decl.members {
        check_attrs(file, &member.attrs, &["since"])?;
        if !names.insert(member.name.text.as_str()) {
            return fail(
                file,
                member.name.pos,
                Code::DuplicateName,
                member.name.text.clone(),
            );
        }
        let value = member_value(member.negative, member.value);
        if !fits(repr, value) {
            return fail(
                file,
                member.name.pos,
                Code::ValueOutOfRange,
                format!("{value} does not fit {}", repr.rust()),
            );
        }
        if !values.insert(value) {
            return fail(
                file,
                member.name.pos,
                Code::DuplicateValue,
                format!("{value} is used twice"),
            );
        }
        let since = member_since(scope, &member.attrs, member.name.pos, item_since)?;
        members.push(Member {
            name: member.name.text.clone(),
            doc: member.doc.clone(),
            value,
            since,
        });
    }
    Ok(Enum {
        library: scope.name.to_string(),
        name: decl.name.text.clone(),
        doc: decl.doc.clone(),
        since: item_since,
        repr,
        members,
    })
}

/// Builds a `bits`' members.
fn bits_decl(scope: &Scope<'_>, decl: &ValuesDecl) -> Result<Bits, Diagnostic> {
    let file = scope.file;
    let repr = match (&decl.repr.qualifier, Int::from_name(&decl.repr.item)) {
        (None, Some(int)) if !int.signed() => int,
        _ => {
            return fail(
                file,
                decl.repr.pos,
                Code::BadUnderlying,
                "bits needs an unsigned integer type",
            );
        }
    };
    if decl.members.is_empty() {
        return fail(
            file,
            decl.name.pos,
            Code::EmptyDeclaration,
            decl.name.text.clone(),
        );
    }
    check_attrs(file, &decl.attrs, &["since"])?;
    let item_since = since_of(file, &decl.attrs, scope.version, item_base(scope.version))?;
    let width = repr.size().saturating_mul(8);
    let mut names = HashSet::new();
    let mut indexes = HashSet::new();
    let mut members = Vec::new();
    for member in &decl.members {
        check_attrs(file, &member.attrs, &["since"])?;
        if member.name.text == "all" || member.name.text == "none" {
            return fail(
                file,
                member.name.pos,
                Code::ReservedMember,
                member.name.text.clone(),
            );
        }
        if !names.insert(member.name.text.as_str()) {
            return fail(
                file,
                member.name.pos,
                Code::DuplicateName,
                member.name.text.clone(),
            );
        }
        if member.negative || member.value >= width {
            return fail(
                file,
                member.name.pos,
                Code::BitIndexOutOfRange,
                format!("bit {} of a {}-bit type", member.value, width),
            );
        }
        if !indexes.insert(member.value) {
            return fail(
                file,
                member.name.pos,
                Code::DuplicateValue,
                format!("bit {} is used twice", member.value),
            );
        }
        let since = member_since(scope, &member.attrs, member.name.pos, item_since)?;
        members.push(Member {
            name: member.name.text.clone(),
            doc: member.doc.clone(),
            value: i128::from(member.value),
            since,
        });
    }
    Ok(Bits {
        library: scope.name.to_string(),
        name: decl.name.text.clone(),
        doc: decl.doc.clone(),
        since: item_since,
        repr,
        members,
    })
}

fn const_decl(scope: &Scope<'_>, decl: &ConstDecl) -> Result<Const, Diagnostic> {
    let file = scope.file;
    check_attrs(file, &decl.attrs, &["since"])?;
    let since = since_of(file, &decl.attrs, scope.version, item_base(scope.version))?;
    let int = if decl.ty.qualifier.is_none() {
        Int::from_name(&decl.ty.item)
    } else {
        None
    };
    let bits = if int.is_none() {
        let symbols = match scope.library_of(&decl.ty)? {
            Some(symbols) => symbols,
            None => &*scope.symbols,
        };
        symbols.bits.get(&decl.ty.item).cloned()
    } else {
        None
    };
    let (kind, value) = match (&decl.value, int, bits) {
        (
            ConstValue::Number {
                negative,
                value,
                pos,
            },
            Some(int),
            _,
        ) => {
            let value = member_value(*negative, *value);
            if !fits(int, value) {
                return fail(file, *pos, Code::ConstOutOfRange, format!("{value}"));
            }
            (ConstKind::Int(int), value)
        }
        (ConstValue::Bits(names), None, Some(bits)) => {
            scope.refuse_newer(decl.ty.pos, &bits.library, bits.since, since, &bits.name)?;
            let mut mask = 0u64;
            for name in names {
                let Some(member) = bits.members.iter().find(|m| m.name == name.text) else {
                    return fail(file, name.pos, Code::BadConstValue, name.text.clone());
                };
                scope.refuse_newer(name.pos, &bits.library, member.since, since, &member.name)?;
                mask |= 1u64 << member.value;
            }
            (ConstKind::Bits(bits), i128::from(mask))
        }
        (ConstValue::Number { pos, .. }, None, Some(_)) => {
            return fail(
                file,
                *pos,
                Code::BadConstValue,
                "a bits constant lists its bits",
            );
        }
        (ConstValue::Bits(names), Some(_), _) => {
            let pos = names.first().map_or(decl.name.pos, |n| n.pos);
            return fail(
                file,
                pos,
                Code::BadConstValue,
                "an integer constant is a number",
            );
        }
        (_, None, None) => {
            return fail(file, decl.ty.pos, Code::BadConstType, decl.ty.item.clone());
        }
    };
    Ok(Const {
        name: decl.name.text.clone(),
        doc: decl.doc.clone(),
        since,
        kind,
        value,
    })
}

fn struct_decl(scope: &Scope<'_>, decl: &StructDecl) -> Result<Struct, Diagnostic> {
    let file = scope.file;
    if decl.sized && scope.transport != Transport::Syscall {
        return fail(
            file,
            decl.name.pos,
            Code::SizedNotAllowed,
            "a sized struct is only for a syscall library",
        );
    }
    if decl.fields.is_empty() {
        return fail(
            file,
            decl.name.pos,
            Code::EmptyDeclaration,
            decl.name.text.clone(),
        );
    }
    check_attrs(file, &decl.attrs, &["since"])?;
    let base = since_of(file, &decl.attrs, scope.version, item_base(scope.version))?;
    let mut names = HashSet::new();
    let mut fields = Vec::new();
    let mut last_since = base;
    for field in &decl.fields {
        check_field(
            scope,
            decl,
            field,
            &mut names,
            base,
            &mut last_since,
            &mut fields,
        )?;
    }
    let laid = layout::struct_layout(fields, decl.sized);
    if laid.size > MAX_FIXED {
        return fail(
            file,
            decl.name.pos,
            Code::TooLarge,
            format!("{} bytes", laid.size),
        );
    }
    let narrow = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    let depth = laid
        .fields
        .iter()
        .map(|f| struct_depth(&f.ty))
        .fold(0, u32::max)
        .saturating_add(1);
    if depth > MAX_STRUCT_DEPTH {
        return fail(
            file,
            decl.name.pos,
            Code::NestingTooDeep,
            format!("structs nest {depth} levels deep"),
        );
    }
    let has_status = laid.fields.iter().any(|f| contains_status(&f.ty));
    Ok(Struct {
        library: scope.name.to_string(),
        name: decl.name.text.clone(),
        doc: decl.doc.clone(),
        since: base,
        depth,
        has_status,
        sized: decl.sized,
        fields: laid.fields,
        size: narrow(laid.size),
        align: narrow(laid.align),
        padding: laid
            .padding
            .iter()
            .map(|(at, len)| (narrow(*at), narrow(*len)))
            .collect(),
        sizes: laid.sizes.iter().map(|(m, s)| (*m, narrow(*s))).collect(),
    })
}

fn check_field(
    scope: &Scope<'_>,
    decl: &StructDecl,
    field: &FieldDecl,
    names: &mut HashSet<String>,
    base: Version,
    last_since: &mut Version,
    fields: &mut Vec<Field>,
) -> Result<(), Diagnostic> {
    let file = scope.file;
    let allowed: &[&str] = if decl.sized { &["since"] } else { &[] };
    check_attrs(file, &field.attrs, allowed)?;
    if !names.insert(field.name.text.clone()) {
        return fail(
            file,
            field.name.pos,
            Code::DuplicateName,
            field.name.text.clone(),
        );
    }
    if decl.sized && field.name.text == "size" {
        return fail(file, field.name.pos, Code::FieldNamedSize, "");
    }
    let ty = scope.resolve_type(&field.ty)?;
    if !is_fixed(&ty) || contains_status(&ty) {
        return fail(
            file,
            field.ty.pos(),
            Code::BadPosition,
            "a struct field must be fixed-size",
        );
    }
    let since = member_since(scope, &field.attrs, field.name.pos, base)?;
    if since < *last_since {
        return fail(file, field.name.pos, Code::SinceNotIncreasing, "");
    }
    scope.refuse_newer_type(field.ty.pos(), &ty, since)?;
    *last_since = since;
    fields.push(Field {
        name: field.name.text.clone(),
        doc: field.doc.clone(),
        ty,
        since,
        offset: 0,
    });
    Ok(())
}

/// The structs of `file` in dependency order: each after every local struct it contains.
fn struct_order<'d>(
    file: &str,
    decls: &'d [&'d StructDecl],
) -> Result<Vec<&'d StructDecl>, Diagnostic> {
    fn dependencies(expr: &TypeExpr, local: &HashSet<&str>, out: &mut Vec<String>) {
        match expr {
            TypeExpr::Named(name)
                if name.qualifier.is_none() && local.contains(name.item.as_str()) =>
            {
                out.push(name.item.clone());
            }
            TypeExpr::Array(inner, _, _) | TypeExpr::Vector(inner, _, _) => {
                dependencies(inner, local, out);
            }
            _ => {}
        }
    }
    let local: HashSet<&str> = decls.iter().map(|d| d.name.text.as_str()).collect();
    let mut state: HashMap<&str, bool> = HashMap::new();
    let mut order = Vec::new();
    // An explicit stack: a chain of many structs is untrusted depth (§12).
    for start in decls {
        if state.contains_key(start.name.text.as_str()) {
            continue;
        }
        let mut stack: Vec<(&StructDecl, Vec<String>)> = Vec::new();
        let deps = |d: &StructDecl| {
            let mut out = Vec::new();
            for field in &d.fields {
                dependencies(&field.ty, &local, &mut out);
            }
            out
        };
        state.insert(start.name.text.as_str(), false);
        stack.push((*start, deps(start)));
        while let Some((decl, pending)) = stack.last_mut() {
            let decl = *decl;
            let Some(next) = pending.pop() else {
                state.insert(decl.name.text.as_str(), true);
                order.push(decl);
                stack.pop();
                continue;
            };
            match state.get(next.as_str()) {
                Some(true) => {}
                Some(false) => {
                    return fail(file, decl.name.pos, Code::RecursiveStruct, next);
                }
                None => {
                    let Some(found) = decls.iter().find(|d| d.name.text == next) else {
                        continue;
                    };
                    state.insert(found.name.text.as_str(), false);
                    stack.push((*found, deps(found)));
                }
            }
        }
    }
    Ok(order)
}

/// Whether `ty` is or contains the `status` enum, which is never a value: only an error.
fn contains_status(ty: &Type) -> bool {
    match ty {
        Type::Enum(e) => e.name == "status",
        Type::Array(elem, _) | Type::Vector(elem, _) => contains_status(elem),
        Type::Struct(s) => s.has_status,
        _ => false,
    }
}

/// What a `syscall` array or vector may hold (§3.1). Integers and handles cross as their
/// in-memory form. A plain struct of integers, enums and bits crosses as its fields, encoded one
/// by one and never in the compiler's layout (ADR-0005 C3); nothing else in it would have one
/// form in both languages.
fn syscall_element_ok(elem: &Type) -> bool {
    match elem {
        Type::Int(_) => true,
        Type::Handle(h) => !h.optional,
        Type::Struct(s) => {
            !s.sized
                && s.fields
                    .iter()
                    .all(|f| matches!(f.ty, Type::Int(_) | Type::Enum(_) | Type::Bits(_)))
        }
        _ => false,
    }
}

/// The bytes a `syscall` vector of structs takes at its bound: the client encodes it into a
/// buffer of that size, so it is held to the 64 KiB an array may have.
fn syscall_records_too_large(ty: &Type) -> bool {
    match ty {
        Type::Vector(elem, bound) => match elem.as_ref() {
            Type::Struct(s) => u64::from(s.size).saturating_mul(u64::from(*bound)) > MAX_FIXED,
            _ => false,
        },
        _ => false,
    }
}

fn check_param_position(
    scope: &Scope<'_>,
    ty: &Type,
    pos: Pos,
    is_result: bool,
) -> Result<(), Diagnostic> {
    let syscall = scope.transport == Transport::Syscall;
    if syscall && syscall_records_too_large(ty) {
        return fail(
            scope.file,
            pos,
            Code::TooLarge,
            "a system-call vector of structs is too large",
        );
    }
    let ok = !contains_status(ty)
        && match ty {
            Type::Struct(s) if s.sized => syscall,
            Type::String(_) | Type::Handle(_) => true,
            Type::Vector(elem, _) => !syscall || syscall_element_ok(elem),
            Type::Array(elem, _) => {
                if syscall {
                    // A handle array crosses as a pointer with no way to state the rights
                    // each handle needs, so only a result may be one (§6); a vector carries
                    // them.
                    syscall_element_ok(elem)
                        && (is_result || !matches!(elem.as_ref(), Type::Handle(_)))
                } else {
                    is_fixed(ty) && !matches!(elem.as_ref(), Type::Handle(_))
                }
            }
            other => is_fixed(other),
        };
    if ok {
        Ok(())
    } else {
        let side = if is_result { "result" } else { "parameter" };
        fail(
            scope.file,
            pos,
            Code::BadPosition,
            format!("this type may not be a {side} of this transport"),
        )
    }
}

fn build_param(
    scope: &Scope<'_>,
    decl: &ParamDecl,
    method_since: Version,
    is_result: bool,
) -> Result<Param, Diagnostic> {
    let file = scope.file;
    let allowed: Vec<&str> = match (scope.transport, is_result) {
        (Transport::Channel, false) => vec!["since", "nonzero"],
        (Transport::Channel, true) => vec!["since"],
        (Transport::Syscall, false) => vec!["nonzero"],
        (Transport::Syscall, true) => vec!["count", "also_on"],
    };
    check_attrs(file, &decl.attrs, &allowed)?;
    let ty = scope.resolve_type(&decl.ty)?;
    check_param_position(scope, &ty, decl.ty.pos(), is_result)?;
    let since = since_of(file, &decl.attrs, scope.version, method_since)?;
    if since < method_since {
        return fail(file, decl.name.pos, Code::BadSince, "older than its method");
    }
    scope.refuse_newer_type(decl.ty.pos(), &ty, since)?;
    let nonzero = if let Some(attr) = find_attr(&decl.attrs, "nonzero") {
        no_args(file, attr)?;
        if !matches!(ty, Type::Int(_) | Type::Bits(_)) {
            return fail(file, attr.pos, Code::NonzeroType, decl.name.text.clone());
        }
        true
    } else {
        false
    };
    let count = if let Some(attr) = find_attr(&decl.attrs, "count") {
        let [AttrArg::Path(parts)] = attr.args.as_slice() else {
            return fail(
                file,
                attr.pos,
                Code::BadAttributeArgs,
                "@count(result.field)",
            );
        };
        let [result, field] = parts.as_slice() else {
            return fail(
                file,
                attr.pos,
                Code::BadAttributeArgs,
                "@count(result.field)",
            );
        };
        if !matches!(ty, Type::Vector(..) | Type::String(_)) {
            return fail(
                file,
                attr.pos,
                Code::BadCount,
                "@count is for a vector or string",
            );
        }
        Some((result.clone(), field.clone()))
    } else {
        None
    };
    let mut also_on = Vec::new();
    if let Some(attr) = find_attr(&decl.attrs, "also_on") {
        if matches!(ty, Type::Vector(..) | Type::String(_)) {
            return fail(
                file,
                attr.pos,
                Code::MisplacedAttribute,
                "@also_on: a failed call does not report a buffer's length",
            );
        }
        if attr.args.is_empty() {
            return fail(file, attr.pos, Code::BadAttributeArgs, "@also_on(error, …)");
        }
        for arg in &attr.args {
            let AttrArg::Path(parts) = arg else {
                return fail(file, attr.pos, Code::BadAttributeArgs, "@also_on(error, …)");
            };
            let [name] = parts.as_slice() else {
                return fail(file, attr.pos, Code::BadAttributeArgs, "@also_on(error, …)");
            };
            if also_on.contains(name) {
                return fail(file, attr.pos, Code::BadAttributeArgs, "listed twice");
            }
            also_on.push(name.clone());
        }
    }
    Ok(Param {
        name: decl.name.text.clone(),
        doc: decl.doc.clone(),
        ty,
        since,
        nonzero,
        count,
        also_on,
    })
}

fn check_since_order(file: &str, params: &[Param], decls: &[ParamDecl]) -> Result<(), Diagnostic> {
    let mut last: Option<Version> = None;
    for (param, decl) in params.iter().zip(decls) {
        if last.is_some_and(|v| param.since < v) {
            return fail(file, decl.name.pos, Code::SinceNotIncreasing, "");
        }
        last = Some(param.since);
    }
    Ok(())
}

/// How many registers a system-call parameter or result takes (§10).
pub fn registers_of(ty: &Type) -> u32 {
    match ty {
        Type::Vector(..) | Type::String(_) => 2,
        _ => 1,
    }
}

/// Which statuses the generated decoding of this `syscall` method can produce (§10).
fn derived_errors(method: &Method) -> Vec<&'static str> {
    let mut needed = Vec::new();
    let mut add = |name: &'static str| {
        if !needed.contains(&name) {
            needed.push(name);
        }
    };
    let handle_rules = |h: &HandleType, add: &mut dyn FnMut(&'static str)| {
        add("bad_handle");
        if h.rights_mask != 0 {
            add("access_denied");
        }
        if h.object.is_some() {
            add("wrong_type");
        }
    };
    for param in &method.params {
        match &param.ty {
            Type::Handle(h) => handle_rules(h, &mut add),
            Type::Int(int) => {
                if !int.is_64() || param.nonzero {
                    add("invalid_args");
                }
            }
            Type::Bool | Type::Enum(_) | Type::Bits(_) => add("invalid_args"),
            Type::Vector(elem, _) => {
                add("invalid_args");
                add("fault");
                if let Type::Handle(h) = elem.as_ref() {
                    handle_rules(h, &mut add);
                }
            }
            Type::String(_) => {
                add("invalid_args");
                add("fault");
            }
            Type::Struct(s) => {
                add("fault");
                if s.sized {
                    add("invalid_args");
                }
            }
            Type::Array(elem, _) => {
                add("fault");
                if let Type::Handle(h) = elem.as_ref() {
                    handle_rules(h, &mut add);
                }
            }
        }
    }
    for result in &method.returns {
        add("fault");
        if matches!(&result.ty, Type::Struct(s) if s.sized) {
            add("invalid_args");
        }
    }
    needed
}

fn build_method(scope: &mut Scope<'_>, decl: &MethodDecl) -> Result<Method, Diagnostic> {
    let file = scope.file;
    let channel = scope.transport == Transport::Channel;
    check_attrs(file, &decl.attrs, &["since", "reserved"])?;
    let reserved = if let Some(attr) = find_attr(&decl.attrs, "reserved") {
        no_args(file, attr)?;
        if channel {
            return fail(file, attr.pos, Code::ReservedNotAllowed, "");
        }
        true
    } else {
        false
    };
    let base = Version {
        major: scope.version.major,
        minor: 0,
    };
    let since = since_of(file, &decl.attrs, scope.version, base)?;
    let max = if channel {
        MAX_CHANNEL_ORDINAL
    } else {
        MAX_SYSCALL_NUMBER
    };
    if decl.ordinal > max || (channel && decl.ordinal == 0) {
        return fail(
            file,
            decl.name.pos,
            Code::BadOrdinal,
            format!("{}", decl.ordinal),
        );
    }
    let ordinal = u32::try_from(decl.ordinal).or(fail(file, decl.pos, Code::BadOrdinal, ""))?;
    if !channel && number_holds_syscall(ordinal) {
        return fail(
            file,
            decl.name.pos,
            Code::BadOrdinal,
            format!(
                "{ordinal}: loading it into EAX would put the two bytes of a syscall instruction (0F 05) into its stub a second time"
            ),
        );
    }
    if decl.oneway && !channel {
        return fail(file, decl.name.pos, Code::OnewayNotAllowed, "only channel");
    }
    if decl.oneway && !matches!(decl.returns, Returns::None) {
        return fail(file, decl.name.pos, Code::OnewayNotAllowed, "no results");
    }
    let never = match &decl.returns {
        Returns::Never(pos) => {
            if channel {
                return fail(file, *pos, Code::NeverNotAllowed, "");
            }
            true
        }
        _ => false,
    };
    let mut names = HashSet::new();
    let result_decls: &[ParamDecl] = match &decl.returns {
        Returns::Values(values) => values,
        _ => &[],
    };
    for param in decl.params.iter().chain(result_decls) {
        if !names.insert(param.name.text.as_str()) {
            return fail(
                file,
                param.name.pos,
                Code::DuplicateParam,
                param.name.text.clone(),
            );
        }
    }
    let mut params = Vec::new();
    for param in &decl.params {
        params.push(build_param(scope, param, since, false)?);
    }
    let mut returns = Vec::new();
    for result in result_decls {
        returns.push(build_param(scope, result, since, true)?);
    }
    if never && params.iter().any(|p| p.ty != Type::Int(Int::U64)) {
        return fail(
            file,
            decl.name.pos,
            Code::BadPosition,
            "a method that does not return can only take u64 arguments: it cannot fail",
        );
    }
    check_since_order(file, &params, &decl.params)?;
    check_since_order(file, &returns, result_decls)?;

    let errors_expected = !(decl.oneway || never);
    let errors_decl = match (&decl.errors, errors_expected) {
        (Some(_), false) => {
            return fail(file, decl.name.pos, Code::ErrorsNotAllowed, "");
        }
        (None, true) => {
            return fail(
                file,
                decl.name.pos,
                Code::MissingErrors,
                decl.name.text.clone(),
            );
        }
        (list, _) => list.as_deref().unwrap_or(&[]),
    };
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    for error in errors_decl {
        if !seen.insert(error.text.as_str()) {
            return fail(file, error.pos, Code::DuplicateError, error.text.clone());
        }
        errors.push(scope.status_member(error)?);
        scope.refuse_newer_status(error, since)?;
    }

    if reserved && !errors.iter().any(|e| e.name == "not_supported") {
        return fail(file, decl.name.pos, Code::ReservedNeedsNotSupported, "");
    }

    let requires = match &decl.requires {
        Requires::None => None,
        Requires::Capability { name, otherwise } => {
            let capability = find_capability(scope, name)?;
            // Capabilities are declared by the library that requires them.
            scope.refuse_newer(
                name.pos,
                scope.name,
                capability.since,
                since,
                &capability.name,
            )?;
            if channel && otherwise.is_some() {
                return fail(file, name.pos, Code::ElseNotAllowed, "");
            }
            let denied = match otherwise {
                Some(status) => scope.status_member(status)?,
                None if channel => StatusRef {
                    name: "access_denied".to_string(),
                    value: match scope
                        .status
                        .as_ref()
                        .and_then(|s| s.members.iter().find(|m| m.name == "access_denied"))
                    {
                        Some(member) => i64::try_from(member.value).unwrap_or(i64::MIN),
                        None => {
                            return fail(
                                file,
                                name.pos,
                                Code::MissingRole,
                                "status has no access_denied",
                            );
                        }
                    },
                },
                None => scope.status_member(&crate::ast::Ident {
                    text: "access_denied".to_string(),
                    pos: name.pos,
                })?,
            };
            if !channel && !errors.iter().any(|e| e.name == denied.name) {
                return fail(file, name.pos, Code::DeniedNotDeclared, denied.name);
            }
            Some(Requirement { capability, denied })
        }
    };

    for result in &returns {
        if let Some((target, field)) = &result.count {
            let Some(found) = returns.iter().find(|r| &r.name == target) else {
                return fail(file, decl.name.pos, Code::BadCount, target.clone());
            };
            // A later field may be absent, and could not say how much was filled.
            let is_u32 = matches!(&found.ty, Type::Struct(s) if s.fields.iter()
                .any(|f| &f.name == field && f.ty == Type::Int(Int::U32) && f.since == s.since));
            if !is_u32 {
                return fail(file, decl.name.pos, Code::BadCount, field.clone());
            }
        }
        for error in &result.also_on {
            if !errors.iter().any(|e| &e.name == error) {
                return fail(file, decl.name.pos, Code::AlsoOnNotDeclared, error.clone());
            }
        }
    }

    let method = Method {
        name: decl.name.text.clone(),
        doc: decl.doc.clone(),
        ordinal,
        oneway: decl.oneway,
        never,
        reserved,
        since,
        params,
        returns,
        errors,
        requires,
    };

    if channel {
        check_channel_limits(scope, decl, &method)?;
    } else {
        check_syscall_shape(scope, decl, &method)?;
    }
    Ok(method)
}

fn find_capability(scope: &Scope<'_>, name: &Dotted) -> Result<Capability, Diagnostic> {
    let text = name.text();
    match scope.capabilities.iter().find(|c| c.name == text) {
        Some(capability) => Ok(capability.clone()),
        None => fail(scope.file, name.pos, Code::UnknownCapability, text),
    }
}

fn check_channel_limits(
    scope: &Scope<'_>,
    decl: &MethodDecl,
    method: &Method,
) -> Result<(), Diagnostic> {
    let file = scope.file;
    let (Some(max_bytes), Some(max_handles)) = scope.limits else {
        return fail(
            file,
            decl.name.pos,
            Code::MissingRole,
            "a channel interface needs max_message_bytes and max_message_handles",
        );
    };
    let newest = scope.version.minor;
    for (params, what) in [(&method.params, "request"), (&method.returns, "response")] {
        if layout::max_message_size(params, newest) > max_bytes {
            return fail(
                file,
                decl.name.pos,
                Code::MessageTooLarge,
                format!("its largest {what} exceeds {max_bytes} bytes"),
            );
        }
        if layout::max_handles(params, newest) > max_handles {
            return fail(
                file,
                decl.name.pos,
                Code::TooManyHandles,
                format!("its largest {what} exceeds {max_handles} handles"),
            );
        }
    }
    Ok(())
}

fn check_syscall_shape(
    scope: &mut Scope<'_>,
    decl: &MethodDecl,
    method: &Method,
) -> Result<(), Diagnostic> {
    let file = scope.file;
    if !scope.syscall_numbers.insert(method.ordinal) {
        return fail(
            file,
            decl.name.pos,
            Code::DuplicateOrdinal,
            format!("{}", method.ordinal),
        );
    }
    let registers: u32 = method
        .params
        .iter()
        .chain(&method.returns)
        .map(|p| registers_of(&p.ty))
        .fold(0, u32::saturating_add);
    if registers > MAX_REGISTERS {
        return fail(
            file,
            decl.name.pos,
            Code::TooManyRegisters,
            format!("{registers} registers"),
        );
    }
    for result in &method.returns {
        let needs_count = matches!(result.ty, Type::Vector(..) | Type::String(_));
        if needs_count && result.count.is_none() {
            return fail(
                file,
                decl.name.pos,
                Code::CountRequired,
                result.name.clone(),
            );
        }
    }
    if !method.reserved {
        for needed in derived_errors(method) {
            if !method.errors.iter().any(|e| e.name == needed) {
                return fail(
                    file,
                    decl.name.pos,
                    Code::UndeclaredDerivedError,
                    format!("{}: {needed}", method.name),
                );
            }
        }
    }
    Ok(())
}

fn interface_decl(scope: &mut Scope<'_>, decl: &InterfaceDecl) -> Result<Interface, Diagnostic> {
    let file = scope.file;
    if decl.methods.is_empty() {
        return fail(
            file,
            decl.name.pos,
            Code::EmptyDeclaration,
            decl.name.text.clone(),
        );
    }
    let mut names = HashSet::new();
    let mut ordinals = HashSet::new();
    let mut methods = Vec::new();
    for method in &decl.methods {
        if !names.insert(method.name.text.as_str()) {
            return fail(
                file,
                method.name.pos,
                Code::DuplicateName,
                method.name.text.clone(),
            );
        }
        if !ordinals.insert(method.ordinal) {
            return fail(
                file,
                method.name.pos,
                Code::DuplicateOrdinal,
                format!("{}", method.ordinal),
            );
        }
        methods.push(build_method(scope, method)?);
    }
    methods.sort_by_key(|m| m.ordinal);
    Ok(Interface {
        name: decl.name.text.clone(),
        doc: decl.doc.clone(),
        methods,
    })
}

/// Checks that generated names cannot collide (§2).
fn check_generated_names(library: &Library, file: &str) -> Result<(), Diagnostic> {
    let pos = Pos { line: 1, column: 1 };
    let mut rust = HashSet::new();
    let mut macros = HashSet::new();
    let macro_prefix = shouty(&names::module(&library.name));
    let mut claim_rust = |name: String| -> Result<(), Diagnostic> {
        if names::is_reserved_pascal(&name) {
            return fail(
                file,
                pos,
                Code::NameCollision,
                format!("{name} is reserved"),
            );
        }
        if !rust.insert(name.clone()) {
            return fail(file, pos, Code::NameCollision, name);
        }
        Ok(())
    };
    let mut claim_macro = |name: String| -> Result<(), Diagnostic> {
        if !macros.insert(name.clone()) {
            return fail(file, pos, Code::NameCollision, name);
        }
        Ok(())
    };
    let is_role = |name: &str| matches!(name, "status" | "rights" | "object_type");
    for e in &library.enums {
        if !is_role(&e.name) {
            claim_rust(pascal(&e.name))?;
        }
        let mut members = HashSet::new();
        for member in &e.members {
            if !members.insert(pascal(&member.name)) {
                return fail(file, pos, Code::NameCollision, member.name.clone());
            }
            claim_macro(format!(
                "{macro_prefix}_{}_{}",
                shouty(&e.name),
                shouty(&member.name)
            ))?;
        }
    }
    for b in &library.bits {
        if !is_role(&b.name) {
            claim_rust(pascal(&b.name))?;
        }
        let mut members = HashSet::new();
        for member in &b.members {
            if !members.insert(shouty(&member.name)) {
                return fail(file, pos, Code::NameCollision, member.name.clone());
            }
            claim_macro(format!(
                "{macro_prefix}_{}_{}",
                shouty(&b.name),
                shouty(&member.name)
            ))?;
        }
        claim_macro(format!("{macro_prefix}_{}_ALL", shouty(&b.name)))?;
    }
    for s in &library.structs {
        claim_rust(pascal(&s.name))?;
        claim_macro(format!("{macro_prefix}_{}_SIZE", shouty(&s.name)))?;
    }
    for c in &library.consts {
        claim_rust(shouty(&c.name))?;
        claim_macro(format!("{macro_prefix}_{}", shouty(&c.name)))?;
    }
    let mut capabilities = HashSet::new();
    for capability in &library.capabilities {
        if !capabilities.insert(pascal(&capability.ident())) {
            return fail(file, pos, Code::NameCollision, capability.name.clone());
        }
    }
    let mut interfaces = HashSet::new();
    for interface in &library.interfaces {
        if interface.name == "types" || !interfaces.insert(interface.name.clone()) {
            return fail(file, pos, Code::NameCollision, interface.name.clone());
        }
        let mut local = HashSet::new();
        for method in &interface.methods {
            // The server trait's `holds` and the client's `transport`. The client's hello is
            // `new`, which is reserved already as a C++ keyword.
            if matches!(method.name.as_str(), "holds" | "transport") {
                return fail(file, pos, Code::NameCollision, method.name.clone());
            }
            let base = pascal(&method.name);
            for suffix in [
                "Error",
                "Request",
                "RequestArgs",
                "Response",
                "ResponseArgs",
                "Failure",
                "Out",
            ] {
                if !local.insert(format!("{base}{suffix}")) {
                    return fail(file, pos, Code::NameCollision, method.name.clone());
                }
            }
        }
    }
    Ok(())
}

fn resolve_version(
    file: &str,
    decl: &VersionDecl,
    versions: Option<&HashMap<String, String>>,
) -> Result<(Version, Option<String>), Diagnostic> {
    match decl {
        VersionDecl::Literal(major, minor, pos) => {
            let (Ok(major), Ok(minor)) = (u16::try_from(*major), u16::try_from(*minor)) else {
                return fail(file, *pos, Code::BadVersion, "a version part exceeds 65535");
            };
            Ok((Version { major, minor }, None))
        }
        VersionDecl::Key(key, pos) => {
            let Some(values) = versions else {
                return fail(file, *pos, Code::VersionKeyMissing, key.clone());
            };
            let Some(value) = values.get(key) else {
                return fail(file, *pos, Code::VersionKeyUnknown, key.clone());
            };
            match parse_version(value) {
                Some(version) => Ok((version, Some(key.clone()))),
                None => fail(file, *pos, Code::VersionKeyMalformed, value.clone()),
            }
        }
    }
}

/// The libraries in dependency order, each after what it uses.
fn order(asts: &[LibraryFile]) -> Result<Vec<usize>, Diagnostic> {
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (index, ast) in asts.iter().enumerate() {
        let text = ast.name.text();
        if ast.name.parts.len() < 2 || ast.name.parts.len() > 4 {
            return fail(&ast.file, ast.name.pos, Code::BadLibraryName, text);
        }
        if by_name.insert(text.clone(), index).is_some() {
            return fail(&ast.file, ast.name.pos, Code::DuplicateLibrary, text);
        }
    }
    let mut order = Vec::new();
    let mut state: HashMap<usize, bool> = HashMap::new();
    for start in 0..asts.len() {
        if state.contains_key(&start) {
            continue;
        }
        state.insert(start, false);
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some((library, next)) = stack.last().copied() {
            let Some(ast) = asts.get(library) else {
                stack.pop();
                continue;
            };
            match ast.uses.get(next) {
                None => {
                    state.insert(library, true);
                    order.push(library);
                    stack.pop();
                }
                Some(used) => {
                    if let Some(top) = stack.last_mut() {
                        top.1 = next.saturating_add(1);
                    }
                    let Some(&target) = by_name.get(&used.text()) else {
                        return fail(&ast.file, used.pos, Code::UnknownLibrary, used.text());
                    };
                    match state.get(&target) {
                        Some(true) => {}
                        Some(false) => {
                            return fail(&ast.file, used.pos, Code::UseCycle, used.text());
                        }
                        None => {
                            state.insert(target, false);
                            stack.push((target, 0));
                        }
                    }
                }
            }
        }
    }
    Ok(order)
}

/// Compiles the given libraries. `versions` is the text of `origin-version.toml`, needed only
/// if a library names a compatibility key.
pub fn compile(inputs: &[Input], versions: Option<&str>) -> Result<Compilation, Diagnostic> {
    let mut asts = Vec::new();
    for input in inputs {
        asts.push(parse::parse(&input.file, &input.text)?);
    }
    let compatibility = versions.map(compatibility_keys);
    let build_order = order(&asts)?;

    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (index, ast) in asts.iter().enumerate() {
        by_name.insert(ast.name.text(), index);
    }

    // Built in dependency order; the symbol tables are indexed by the input's position.
    let mut symbols: Vec<Option<Symbols>> = (0..asts.len()).map(|_| None).collect();
    let mut comp = Compilation {
        libraries: Vec::new(),
        status: None,
        rights: None,
        object_type: None,
        max_message_bytes: None,
        max_message_handles: None,
    };
    // Where each role was declared, so availability through `use` can be checked.
    let mut role_library: HashMap<&'static str, String> = HashMap::new();

    for &index in &build_order {
        let Some(ast) = asts.get(index) else {
            continue;
        };
        let library = build_library(
            ast,
            &by_name,
            &mut symbols,
            index,
            compatibility.as_ref(),
            &mut comp,
            &mut role_library,
        )?;
        comp.libraries.push(library);
    }
    Ok(comp)
}

fn item_name(item: &Item) -> Option<&crate::ast::Ident> {
    match item {
        Item::Const(d) => Some(&d.name),
        Item::Enum(d) | Item::Bits(d) => Some(&d.name),
        Item::Struct(d) => Some(&d.name),
        Item::Interface(d) => Some(&d.name),
        Item::Capability(_) => None,
    }
}

#[allow(clippy::too_many_lines)]
fn build_library(
    ast: &LibraryFile,
    by_name: &HashMap<String, usize>,
    symbols: &mut [Option<Symbols>],
    index: usize,
    compatibility: Option<&HashMap<String, String>>,
    comp: &mut Compilation,
    role_library: &mut HashMap<&'static str, String>,
) -> Result<Library, Diagnostic> {
    let file = ast.file.as_str();
    let name = ast.name.text();
    let (version, version_key) = resolve_version(file, &ast.version, compatibility)?;

    // `use`: by last component, unique.
    let mut uses_map: HashMap<String, usize> = HashMap::new();
    let mut uses = Vec::new();
    for used in &ast.uses {
        let text = used.text();
        if uses.contains(&text) {
            return fail(file, used.pos, Code::DuplicateUse, text);
        }
        let last = used.parts.last().cloned().unwrap_or_default();
        let Some(&target) = by_name.get(&text) else {
            return fail(file, used.pos, Code::UnknownLibrary, text);
        };
        if uses_map.insert(last, target).is_some() {
            return fail(file, used.pos, Code::DuplicateUse, text);
        }
        uses.push(text);
    }

    // One namespace for items.
    let mut seen = HashSet::new();
    for item in &ast.items {
        if let Some(item_name) = item_name(item) {
            if names::is_type_word(&item_name.text) {
                return fail(
                    file,
                    item_name.pos,
                    Code::ReservedWord,
                    item_name.text.clone(),
                );
            }
            if !seen.insert(item_name.text.as_str()) {
                return fail(
                    file,
                    item_name.pos,
                    Code::DuplicateName,
                    item_name.text.clone(),
                );
            }
        }
    }

    let mut local = Symbols::default();
    for item in &ast.items {
        if let Item::Interface(d) = item {
            local.interfaces.insert(d.name.text.clone());
        }
    }
    let available = |role: &str, role_library: &HashMap<&'static str, String>| {
        role_library
            .get(role)
            .is_some_and(|owner| owner == &name || uses.contains(owner))
    };

    let mut enums = Vec::new();
    let mut bits = Vec::new();
    let mut consts = Vec::new();
    let mut structs = Vec::new();
    let mut capabilities = Vec::new();
    let mut interfaces = Vec::new();

    let mut scope_status = None;
    let mut scope_rights = None;
    let mut scope_object = None;
    // Roles from used libraries.
    if available("status", role_library) {
        scope_status.clone_from(&comp.status);
    }
    if available("rights", role_library) {
        scope_rights.clone_from(&comp.rights);
    }
    if available("object_type", role_library) {
        scope_object.clone_from(&comp.object_type);
    }
    let mut limits = (None, None);
    if available("max_message_bytes", role_library) {
        limits.0 = comp.max_message_bytes;
    }
    if available("max_message_handles", role_library) {
        limits.1 = comp.max_message_handles;
    }

    // Enums and bits first: nothing they depend on.
    for item in &ast.items {
        let scope = Scope {
            file,
            name: &name,
            transport: ast.transport,
            version,
            symbols: &mut local,
            uses: &uses_map,
            used: symbols,
            status: None,
            rights: None,
            object_type: None,
            limits: (None, None),
            capabilities: Vec::new(),
            syscall_numbers: HashSet::new(),
        };
        match item {
            Item::Enum(decl) => {
                let built = Rc::new(enum_decl(&scope, decl)?);
                drop(scope);
                let role_error = |message: &str| {
                    fail::<()>(file, decl.name.pos, Code::BadRole, message.to_string())
                };
                match built.name.as_str() {
                    "status" => {
                        if comp.status.is_some() {
                            return fail(file, decl.name.pos, Code::DuplicateRole, "status");
                        }
                        let mut has_ok = false;
                        for member in &built.members {
                            if member.name == "ok" {
                                if member.value != 0 {
                                    role_error("ok must be 0")?;
                                }
                                has_ok = true;
                            } else if member.value >= 0 {
                                role_error("every status but ok is negative")?;
                            }
                        }
                        if built.repr != Int::I64 || !has_ok {
                            role_error("status is an i64 enum with ok = 0")?;
                        }
                        comp.status = Some(built.clone());
                        scope_status = Some(built.clone());
                        role_library.insert("status", name.clone());
                    }
                    "object_type" => {
                        if comp.object_type.is_some() {
                            return fail(file, decl.name.pos, Code::DuplicateRole, "object_type");
                        }
                        if built.repr != Int::U32
                            || built
                                .members
                                .iter()
                                .any(|m| m.name == "any" || m.value == 0)
                        {
                            role_error("object_type is a u32 enum, with no any and no 0")?;
                        }
                        comp.object_type = Some(built.clone());
                        scope_object = Some(built.clone());
                        role_library.insert("object_type", name.clone());
                    }
                    _ => {}
                }
                local.enums.insert(built.name.clone(), built.clone());
                enums.push(built);
            }
            Item::Bits(decl) => {
                let built = Rc::new(bits_decl(&scope, decl)?);
                drop(scope);
                if built.name == "rights" {
                    if comp.rights.is_some() {
                        return fail(file, decl.name.pos, Code::DuplicateRole, "rights");
                    }
                    if built.repr != Int::U32 {
                        return fail(file, decl.name.pos, Code::BadRole, "rights is u32 bits");
                    }
                    comp.rights = Some(built.clone());
                    scope_rights = Some(built.clone());
                    role_library.insert("rights", name.clone());
                }
                local.bits.insert(built.name.clone(), built.clone());
                bits.push(built);
            }
            Item::Capability(decl) => {
                let capability_name = decl.name.text();
                if decl.name.parts.len() < 2 {
                    return fail(
                        file,
                        decl.name.pos,
                        Code::BadLibraryName,
                        "a capability is a dotted name",
                    );
                }
                if capabilities
                    .iter()
                    .any(|c: &Capability| c.name == capability_name)
                {
                    return fail(
                        file,
                        decl.name.pos,
                        Code::DuplicateCapability,
                        capability_name,
                    );
                }
                check_attrs(file, &decl.attrs, &["since"])?;
                capabilities.push(Capability {
                    name: capability_name,
                    doc: decl.doc.clone(),
                    since: since_of(file, &decl.attrs, version, item_base(version))?,
                });
            }
            _ => {}
        }
    }

    // Constants next: a bound or a bits expression needs them.
    for item in &ast.items {
        if let Item::Const(decl) = item {
            let scope = Scope {
                file,
                name: &name,
                transport: ast.transport,
                version,
                symbols: &mut local,
                uses: &uses_map,
                used: symbols,
                status: None,
                rights: None,
                object_type: None,
                limits: (None, None),
                capabilities: Vec::new(),
                syscall_numbers: HashSet::new(),
            };
            let built = const_decl(&scope, decl)?;
            drop(scope);
            match built.name.as_str() {
                "max_message_bytes" => {
                    if comp.max_message_bytes.is_some() {
                        return fail(file, decl.name.pos, Code::DuplicateRole, built.name);
                    }
                    if !matches!(built.kind, ConstKind::Int(_)) {
                        return fail(file, decl.name.pos, Code::BadRole, built.name);
                    }
                    let value = u64::try_from(built.value).unwrap_or(0);
                    comp.max_message_bytes = Some(value);
                    limits.0 = Some(value);
                    role_library.insert("max_message_bytes", name.clone());
                }
                "max_message_handles" => {
                    if comp.max_message_handles.is_some() {
                        return fail(file, decl.name.pos, Code::DuplicateRole, built.name);
                    }
                    if !matches!(built.kind, ConstKind::Int(_)) {
                        return fail(file, decl.name.pos, Code::BadRole, built.name);
                    }
                    let value = u64::try_from(built.value).unwrap_or(0);
                    comp.max_message_handles = Some(value);
                    limits.1 = Some(value);
                    role_library.insert("max_message_handles", name.clone());
                }
                _ => {}
            }
            local.consts.insert(built.name.clone(), built.clone());
            consts.push(built);
        }
    }

    // Structs in dependency order.
    let struct_decls: Vec<&StructDecl> = ast
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(decl) => Some(decl),
            _ => None,
        })
        .collect();
    let ordered = struct_order(file, &struct_decls)?;
    let mut built_structs: HashMap<&str, Rc<Struct>> = HashMap::new();
    for decl in ordered {
        let scope = Scope {
            file,
            name: &name,
            transport: ast.transport,
            version,
            symbols: &mut local,
            uses: &uses_map,
            used: symbols,
            status: None,
            rights: scope_rights.clone(),
            object_type: scope_object.clone(),
            limits: (None, None),
            capabilities: Vec::new(),
            syscall_numbers: HashSet::new(),
        };
        let built = Rc::new(struct_decl(&scope, decl)?);
        drop(scope);
        local.structs.insert(built.name.clone(), built.clone());
        built_structs.insert(decl.name.text.as_str(), built);
    }
    // Declaration order for output.
    for decl in &struct_decls {
        if let Some(built) = built_structs.get(decl.name.text.as_str()) {
            structs.push(built.clone());
        }
    }

    // Interfaces last: they see everything.
    {
        let mut scope = Scope {
            file,
            name: &name,
            transport: ast.transport,
            version,
            symbols: &mut local,
            uses: &uses_map,
            used: symbols,
            status: scope_status.clone(),
            rights: scope_rights.clone(),
            object_type: scope_object.clone(),
            limits,
            capabilities: capabilities.clone(),
            syscall_numbers: HashSet::new(),
        };
        for item in &ast.items {
            if let Item::Interface(decl) = item {
                interfaces.push(interface_decl(&mut scope, decl)?);
            }
        }
    }

    if ast.transport == Transport::Channel {
        // A channel library needs the roles even if no method names them directly: refusals
        // use status (§7.8), and limits bound every message.
        for (role, present) in [
            ("status", scope_status.is_some()),
            ("max_message_bytes", limits.0.is_some()),
            ("max_message_handles", limits.1.is_some()),
        ] {
            if !present {
                return fail(
                    file,
                    ast.name.pos,
                    Code::MissingRole,
                    format!("a channel library needs {role}"),
                );
            }
        }
    }

    let library = Library {
        name: name.clone(),
        doc: ast.doc.clone(),
        transport: ast.transport,
        version,
        version_key,
        uses,
        consts,
        enums,
        bits,
        structs,
        capabilities,
        interfaces,
    };
    check_generated_names(&library, file)?;
    if let Some(slot) = symbols.get_mut(index) {
        *slot = Some(local);
    }
    Ok(library)
}
