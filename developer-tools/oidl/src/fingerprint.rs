//! The ABI fingerprint and the compatibility gate (`docs/spec/OIDL.md` §13).
//!
//! A library's **canonical description** at one minor is text with one fact per line. Comments
//! are never part of it, so documentation cannot change a fingerprint; order is canonical
//! everywhere order is not ABI, so reordering declarations cannot change one either. Each minor
//! has its own digest, which never changes once recorded: additions show up as a new minor,
//! and anything else shows up as a changed one.

use core::fmt::Write as _;
use std::collections::BTreeSet;

use origin_crypto::Sha256;

use crate::layout::HANDLE_BITS;
use crate::model::{
    Bits, Compilation, ConstKind, Enum, Library, Method, Param, Struct, Transport, Type, Version,
};

/// The description's own format version. Format 2 adds the `handle` line (decision item 237).
pub const FORMAT: &str = "oidl-fingerprint 2";

/// The interface id of the hello (§7.7).
pub fn interface_id(library: &str, interface: &str) -> u64 {
    let mut hash = Sha256::new();
    hash.update(b"origin.oidl.interface.v1\0");
    hash.update(format!("{library}.{interface}").as_bytes());
    let digest = hash.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(digest.get(..8).unwrap_or(&[0; 8]));
    u64::from_le_bytes(first)
}

/// How a type is rendered in a description. A type of this library is named: its own lines
/// are in the description. A type of another library is expanded, because that library's own
/// changes would otherwise alter this library's messages unseen. Base members and the layout
/// are what decide those messages; members added to it later are its own business (§13).
fn type_text(ty: &Type, library: &str) -> String {
    match ty {
        Type::Bool => "bool".to_string(),
        Type::Int(int) => int.rust().to_string(),
        Type::Enum(e) if e.library == library => format!("enum({})", e.name),
        Type::Enum(e) => {
            let mut members: Vec<&crate::model::Member> =
                e.members.iter().filter(|m| m.since == e.since).collect();
            members.sort_by_key(|m| m.value);
            let list: Vec<String> = members
                .iter()
                .map(|m| format!("{}={}", m.name, m.value))
                .collect();
            format!(
                "enum({}.{},{},[{}])",
                e.library,
                e.name,
                e.repr.rust(),
                list.join(",")
            )
        }
        Type::Bits(b) if b.library == library => format!("bits({})", b.name),
        Type::Bits(b) => {
            let mut members: Vec<&crate::model::Member> =
                b.members.iter().filter(|m| m.since == b.since).collect();
            members.sort_by_key(|m| m.value);
            let list: Vec<String> = members
                .iter()
                .map(|m| format!("{}={}", m.name, m.value))
                .collect();
            format!(
                "bits({}.{},{},[{}])",
                b.library,
                b.name,
                b.repr.rust(),
                list.join(",")
            )
        }
        Type::Struct(s) if s.library == library => format!("struct({},{})", s.name, s.size),
        Type::Struct(s) => {
            // Expanded so a change of layout in a used library shows here.
            let fields: Vec<String> = s
                .fields
                .iter()
                .map(|f| format!("{}:{}@{}", f.name, type_text(&f.ty, &s.library), f.offset))
                .collect();
            format!(
                "struct({}.{},{},[{}])",
                s.library,
                s.name,
                s.size,
                fields.join(";")
            )
        }
        Type::Array(elem, count) => format!("array({},{count})", type_text(elem, library)),
        Type::Vector(elem, bound) => format!("vector({},{bound})", type_text(elem, library)),
        Type::String(bound) => format!("string({bound})"),
        Type::Handle(h) => format!(
            "{}handle({},{})",
            if h.optional { "optional " } else { "" },
            h.object.as_deref().unwrap_or("any"),
            h.rights.join("|")
        ),
    }
}

fn write_enum(out: &mut String, e: &Enum, minor: u16) {
    if e.since.minor > minor {
        return;
    }
    let _ = writeln!(out, "enum {} {} since={}", e.name, e.repr.rust(), e.since);
    let mut members: Vec<_> = e
        .members
        .iter()
        .filter(|m| m.since.minor <= minor)
        .collect();
    members.sort_by_key(|m| m.value);
    for member in members {
        let _ = writeln!(
            out,
            " {} {} since={}",
            member.name, member.value, member.since
        );
    }
}

fn write_bits(out: &mut String, b: &Bits, minor: u16) {
    if b.since.minor > minor {
        return;
    }
    let _ = writeln!(out, "bits {} {} since={}", b.name, b.repr.rust(), b.since);
    let mut members: Vec<_> = b
        .members
        .iter()
        .filter(|m| m.since.minor <= minor)
        .collect();
    members.sort_by_key(|m| m.value);
    for member in members {
        let _ = writeln!(
            out,
            " {} {} since={}",
            member.name, member.value, member.since
        );
    }
}

fn write_struct(out: &mut String, s: &Struct, minor: u16) {
    if s.since.minor > minor {
        return;
    }
    let fields: Vec<_> = s.fields.iter().filter(|f| f.since.minor <= minor).collect();
    let size = if s.sized { s.size_at(minor) } else { s.size };
    let _ = writeln!(
        out,
        "struct {} {} size={size} align={} since={}",
        s.name,
        if s.sized { "sized" } else { "plain" },
        s.align_at(minor),
        s.since
    );
    for field in fields {
        let _ = writeln!(
            out,
            " {} {} at={} since={}",
            field.name,
            type_text(&field.ty, &s.library),
            field.offset,
            field.since
        );
    }
}

/// How a parameter's or result's type is rendered at `minor`. A `sized struct` of this library
/// is named with its size at that minor: a field it gains in a later minor is not part of this
/// one (§9, §13), so its size must not reach this minor's digest either. Only a parameter or
/// result can be a sized struct, never a field or an element, so this is the one place it
/// needs the minor.
fn param_type_text(ty: &Type, library: &str, minor: u16) -> String {
    match ty {
        Type::Struct(s) if s.sized && s.library == library => {
            format!("struct({},{})", s.name, s.size_at(minor))
        }
        other => type_text(other, library),
    }
}

fn write_params(out: &mut String, label: &str, params: &[Param], minor: u16, library: &str) {
    for param in params.iter().filter(|p| p.since.minor <= minor) {
        let _ = write!(
            out,
            " {label} {} {} since={}",
            param.name,
            param_type_text(&param.ty, library, minor),
            param.since
        );
        if param.nonzero {
            let _ = write!(out, " nonzero");
        }
        if let Some((result, field)) = &param.count {
            let _ = write!(out, " count={result}.{field}");
        }
        if !param.also_on.is_empty() {
            let mut also = param.also_on.clone();
            also.sort();
            let _ = write!(out, " also_on={}", also.join(","));
        }
        let _ = writeln!(out);
    }
}

fn write_method(
    out: &mut String,
    library: &str,
    interface: &str,
    method: &Method,
    minor: u16,
    numbers: bool,
) {
    if method.since.minor > minor {
        return;
    }
    let ordinal = if numbers {
        method.ordinal.to_string()
    } else {
        "-".to_string()
    };
    let _ = writeln!(
        out,
        "method {interface}.{} ordinal={ordinal} since={} oneway={} never={} reserved={}",
        method.name, method.since, method.oneway, method.never, method.reserved
    );
    write_params(out, "in", &method.params, minor, library);
    write_params(out, "out", &method.returns, minor, library);
    let mut errors: Vec<String> = method
        .errors
        .iter()
        .map(|e| format!(" error {}", e.name))
        .collect();
    errors.sort();
    for error in errors {
        let _ = writeln!(out, "{error}");
    }
    match &method.requires {
        Some(requirement) => {
            let _ = writeln!(
                out,
                " requires {} else={}",
                requirement.capability.name, requirement.denied.name
            );
        }
        None => {
            let _ = writeln!(out, " requires none");
        }
    }
}

/// The canonical description of `library` at `minor`. With `numbers`, `syscall` numbers are
/// included (§13); `channel` ordinals are always ABI and are always included.
pub fn describe(library: &Library, minor: u16, numbers: bool) -> String {
    describe_with(library, minor, numbers, HANDLE_BITS)
}

/// [`describe`], for a handle `handle_bits` wide: the generator's own width, or another one a
/// test plants to show that the width reaches the digest.
pub(crate) fn describe_with(
    library: &Library,
    minor: u16,
    numbers: bool,
    handle_bits: u32,
) -> String {
    let channel = library.transport == Transport::Channel;
    let mut out = String::new();
    let _ = writeln!(out, "{FORMAT}");
    let _ = writeln!(out, "library {}", library.name);
    let _ = writeln!(out, "major {}", library.version.major);
    let _ = writeln!(
        out,
        "transport {}",
        if channel { "channel" } else { "syscall" }
    );
    // A handle's width is not in any declaration: the specification sets it (§10), the generator
    // emits it, and so it is a fact of every library at every minor (decision item 237).
    let _ = writeln!(out, "handle {handle_bits}");
    let mut consts: Vec<String> = library
        .consts
        .iter()
        .filter(|c| c.since.minor <= minor)
        .map(|c| {
            let kind = match &c.kind {
                ConstKind::Int(int) => int.rust().to_string(),
                ConstKind::Bits(b) => format!("bits({}.{})", b.library, b.name),
            };
            format!("const {} {kind} {} since={}", c.name, c.value, c.since)
        })
        .collect();
    consts.sort();
    for line in consts {
        let _ = writeln!(out, "{line}");
    }
    let mut enums: Vec<_> = library.enums.iter().collect();
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    for e in enums {
        write_enum(&mut out, e, minor);
    }
    let mut bits: Vec<_> = library.bits.iter().collect();
    bits.sort_by(|a, b| a.name.cmp(&b.name));
    for b in bits {
        write_bits(&mut out, b, minor);
    }
    let mut structs: Vec<_> = library.structs.iter().collect();
    structs.sort_by(|a, b| a.name.cmp(&b.name));
    for s in structs {
        write_struct(&mut out, s, minor);
    }
    let mut capabilities: Vec<&crate::model::Capability> = library
        .capabilities
        .iter()
        .filter(|c| c.since.minor <= minor)
        .collect();
    capabilities.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    for capability in capabilities {
        let _ = writeln!(
            out,
            "capability {} since={}",
            capability.name, capability.since
        );
    }
    let mut interfaces: Vec<&crate::model::Interface> = library.interfaces.iter().collect();
    interfaces.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    for interface in interfaces {
        for method in &interface.methods {
            write_method(
                &mut out,
                &library.name,
                &interface.name,
                method,
                minor,
                numbers || channel,
            );
        }
    }
    out
}

/// The fingerprints of one minor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinorPrint {
    /// `major.minor`.
    pub version: Version,
    /// The stable fingerprint: what the compatibility gate compares.
    pub stable: [u8; 32],
    /// The release fingerprint: also covers `syscall` numbers.
    pub release: [u8; 32],
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The minors at which a library's description can change, so `version 1.65535` does not make
/// the tool render it 65,536 times.
fn changing_minors(library: &Library) -> BTreeSet<u16> {
    let mut minors = BTreeSet::from([0]);
    minors.extend(library.consts.iter().map(|c| c.since.minor));
    minors.extend(library.capabilities.iter().map(|c| c.since.minor));
    for e in &library.enums {
        minors.insert(e.since.minor);
        minors.extend(e.members.iter().map(|m| m.since.minor));
    }
    for b in &library.bits {
        minors.insert(b.since.minor);
        minors.extend(b.members.iter().map(|m| m.since.minor));
    }
    for s in &library.structs {
        minors.insert(s.since.minor);
        minors.extend(s.fields.iter().map(|f| f.since.minor));
    }
    for interface in &library.interfaces {
        for method in &interface.methods {
            minors.insert(method.since.minor);
            minors.extend(
                method
                    .params
                    .iter()
                    .chain(&method.returns)
                    .map(|p| p.since.minor),
            );
        }
    }
    minors
}

/// A library's fingerprints, one per minor up to its current one.
pub fn fingerprints(library: &Library) -> Vec<MinorPrint> {
    fingerprints_with(library, HANDLE_BITS)
}

/// [`fingerprints`], for a handle `handle_bits` wide (see [`describe_with`]).
pub(crate) fn fingerprints_with(library: &Library, handle_bits: u32) -> Vec<MinorPrint> {
    let current = library.version;
    let changing = changing_minors(library);
    let mut stable = [0u8; 32];
    let mut release = [0u8; 32];
    (0..=current.minor)
        .map(|minor| {
            if changing.contains(&minor) {
                stable =
                    Sha256::digest(describe_with(library, minor, false, handle_bits).as_bytes());
                release =
                    Sha256::digest(describe_with(library, minor, true, handle_bits).as_bytes());
            }
            MinorPrint {
                version: Version {
                    major: current.major,
                    minor,
                },
                stable,
                release,
            }
        })
        .collect()
}

/// The contents of `abi/<library>.fingerprint`.
pub fn fingerprint_file(library: &Library) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {FORMAT} {}", library.name);
    for print in fingerprints(library) {
        let _ = writeln!(
            out,
            "{} {} stable=sha256:{} release=sha256:{}",
            library.name,
            print.version,
            hex(&print.stable),
            hex(&print.release)
        );
    }
    out
}

/// One line of a lock file.
fn lock_line(library: &str, version: Version, digest: &[u8; 32]) -> String {
    format!("{library} {version} sha256:{}", hex(digest))
}

/// What a lock-file comparison found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockFinding {
    /// A recorded minor's fingerprint changed.
    Changed {
        /// Which library.
        library: String,
        /// Which minor.
        version: Version,
    },
    /// A recorded minor no longer exists in the definition: the library's major went
    /// backwards, its minor went down, or the library is not among the inputs.
    Gone {
        /// Which library.
        library: String,
        /// Which minor.
        version: Version,
    },
    /// A current minor has no record yet.
    Unrecorded {
        /// Which library.
        library: String,
        /// Which minor.
        version: Version,
    },
    /// A line of the lock file is not in the format this tool writes.
    Malformed {
        /// 1-based line number.
        line: usize,
    },
}

/// The result of comparing a lock file against the definitions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LockReport {
    /// Changes that fail the gate.
    pub errors: Vec<LockFinding>,
    /// Changes that are reported but allowed (major 0).
    pub warnings: Vec<LockFinding>,
    /// Minors with no record, to be added with `--update`.
    pub unrecorded: Vec<LockFinding>,
}

fn is_digest(text: &str) -> bool {
    text.strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The lock's lines, and the 1-based numbers of those that are malformed.
fn parse_lock(lock: &str) -> (Vec<(String, Version, String)>, Vec<usize>) {
    let mut lines = Vec::new();
    let mut malformed = Vec::new();
    for (index, line) in lock.lines().enumerate() {
        let number = index.saturating_add(1);
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(name), Some(version), Some(digest), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            malformed.push(number);
            continue;
        };
        let Some((major, minor)) = version.split_once('.') else {
            malformed.push(number);
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u16>(), minor.parse::<u16>()) else {
            malformed.push(number);
            continue;
        };
        if !is_digest(digest) {
            malformed.push(number);
            continue;
        }
        lines.push((
            name.to_string(),
            Version { major, minor },
            digest.to_string(),
        ));
    }
    (lines, malformed)
}

/// Compares `lock` with the definitions (§13). A recorded library that is not among the inputs
/// is reported as gone, so the gate must be run over every library the lock names.
pub fn check_lock(comp: &Compilation, lock: &str) -> LockReport {
    let (recorded, malformed) = parse_lock(lock);
    let mut report = LockReport::default();
    report.errors.extend(
        malformed
            .into_iter()
            .map(|line| LockFinding::Malformed { line }),
    );
    for library in &comp.libraries {
        let prints = fingerprints(library);
        for print in &prints {
            let found = recorded
                .iter()
                .find(|(name, version, _)| name == &library.name && *version == print.version);
            match found {
                None => report.unrecorded.push(LockFinding::Unrecorded {
                    library: library.name.clone(),
                    version: print.version,
                }),
                Some((_, _, digest)) => {
                    let expected = format!("sha256:{}", hex(&print.stable));
                    if *digest != expected {
                        let finding = LockFinding::Changed {
                            library: library.name.clone(),
                            version: print.version,
                        };
                        if library.version.major == 0 {
                            report.warnings.push(finding);
                        } else {
                            report.errors.push(finding);
                        }
                    }
                }
            }
        }
        // A recorded minor that no longer exists: it was removed, or the version went
        // backwards. At major 0 a removal within the major is only reported; a major going
        // down is always an error, since it would let a changed ABI pass as the old one.
        for (name, version, _) in &recorded {
            if name != &library.name {
                continue;
            }
            let backwards = version.major > library.version.major;
            let missing = version.major == library.version.major
                && !prints.iter().any(|p| p.version == *version);
            if backwards || missing {
                let finding = LockFinding::Gone {
                    library: library.name.clone(),
                    version: *version,
                };
                if library.version.major == 0 && !backwards {
                    report.warnings.push(finding);
                } else {
                    report.errors.push(finding);
                }
            }
        }
    }
    // A library the lock records that is not among the inputs at all.
    for (name, version, _) in &recorded {
        if !comp.libraries.iter().any(|l| &l.name == name) {
            report.errors.push(LockFinding::Gone {
                library: name.clone(),
                version: *version,
            });
        }
    }
    report
}

/// The lock file text for the definitions. Records every current minor.
pub fn render_lock(comp: &Compilation) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# {FORMAT}: one line per library and minor (docs/spec/OIDL.md §13)"
    );
    for library in &comp.libraries {
        for print in fingerprints(library) {
            let _ = writeln!(
                out,
                "{}",
                lock_line(&library.name, print.version, &print.stable)
            );
        }
    }
    out
}
