//! `origin.core`, described for the kernel at build time (ADR-0011 Amendment 2, L2 and L4 to
//! L7; task S4-2). No ONBF profile: until the Loader Service exists (milestone M6) the kernel
//! carries `origin.core` in its own image, as it carries its programs ([`crate::carried`]), and
//! reads no image at run time.
//!
//! The input is what `rust-lld` wrote for the crate `runtime/core`: a shared object named
//! `origin.core`. It is a shared object only so that a program can be linked against it and
//! have its imports named by the linker. Nothing of its dynamic table, its symbol table or its
//! headers reaches the kernel or a process: no loader exists to read them. [`describe`] takes
//! from it the bytes of its one read-execute segment, and for each export its name and place,
//! and writes down what the kernel needs and nothing else.
//!
//! The image must be the generated stubs and nothing else (L4), with no writable data and
//! nothing to relocate (L2). So [`describe`] refuses:
//!
//! - any relocation, and any import;
//! - a writable segment that holds anything but the linker's own dynamic table;
//! - a number of read-execute segments other than one;
//! - an export the generator's list does not name, a listed export that is missing, and an
//!   export that is not a public function inside the code;
//! - a stub whose bytes hold anything but exactly one `syscall` instruction's two bytes, and
//!   that pair of bytes anywhere else in the code, at any alignment;
//! - a byte of the code that belongs to no stub and is not the linker's padding.
//!
//! The pair of bytes is `0F 05` (Intel SDM Volume 2, `SYSCALL`; AMD APM Volume 3, `SYSCALL`).
//! Counting the pair, and not decoded instructions, is the stricter rule: it also refuses a
//! stub in which the pair appears inside another instruction, where a jump into its middle
//! would find a `syscall` the kernel's table does not hold. The offset of each stub's pair is
//! the offset the kernel accepts that call number from (L7); `cargo xtask`'s gate works the
//! same offset out again from a disassembly and compares.
//!
//! [`Listing`] is the generator's list of what `origin.core` exports (`core/<library>.exports`,
//! `docs/spec/OIDL.md` §11): each method's number, C symbol, name and the version it arrived
//! in, under the release fingerprint of the definition. The description carries that
//! fingerprint, and the kernel is compiled only if it equals its own dispatcher's.
//!
//! The same linker output and the same list always give the same files, byte for byte
//! (CLAUDE.md §75).

use core::fmt;

use origin_crypto::Sha256;

use crate::elf::{self, ElfError, Linking, Permissions};
use crate::sign::hex;

/// The library's name: its `DT_SONAME`, and what a program's `DT_NEEDED` says.
pub const LIBRARY: &str = "origin.core";
/// The name its description has in the kernel, and its files' stem.
pub const NAME: &str = "origin_core";
/// What follows `# ` on the first line of the generator's list, before the library's name.
const LIST_FORMAT: &str = "oidl-core-exports 1";
/// The two bytes of a `syscall` instruction.
const SYSCALL: [u8; 2] = [0x0F, 0x05];
/// What `rust-lld` fills the space between functions with: `int3`.
const PADDING: u8 = 0xCC;

/// One line of the generator's list: one method's entry point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// The call number.
    pub number: u64,
    /// The C symbol `origin.core` exports for it.
    pub symbol: String,
    /// The method's name: the kernel's name for its pin.
    pub method: String,
    /// The version of the library the method arrived in: `(major, minor)`.
    pub since: (u16, u16),
}

/// The generator's list of what `origin.core` exports for one library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// The library the entry points are generated from.
    pub library: String,
    /// The release fingerprint of its definition (`docs/spec/OIDL.md` §13).
    pub release: [u8; 32],
    /// The entry points, in number order.
    pub entries: Vec<Listed>,
}

/// One export of `origin.core`, as the kernel binds an import to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreExport {
    /// Its name.
    pub name: String,
    /// Where it starts, from the image's base.
    pub offset: u64,
    /// The version its method arrived in.
    pub since: (u16, u16),
}

/// Where one call's `syscall` instruction is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorePin {
    /// The method's name.
    pub method: String,
    /// Its call number.
    pub number: u64,
    /// The instruction's offset from the image's base.
    pub offset: u64,
}

/// `origin.core`, laid out for the kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreLib {
    /// The bytes its code maps: a whole number of pages. The image's base is where its first
    /// page goes.
    pub span: u64,
    /// The code: the stubs, with the linker's padding between them. The rest of `span` is zero.
    pub code: Vec<u8>,
    /// The exports, sorted by name: an import names one by its place here.
    pub exports: Vec<CoreExport>,
    /// Each call's `syscall`, in number order.
    pub pins: Vec<CorePin>,
    /// The release fingerprint of the definition the stubs were generated from.
    pub release: [u8; 32],
    /// SHA-256 of the linker output it was made from, for the record.
    pub source_digest: [u8; 32],
}

/// Why `origin.core` could not be described.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreError {
    /// The linker output breaks the conversion contract (ONBF §6.1.2).
    Elf(ElfError),
    /// The generator's list does not read: the value says what is wrong, and on which line.
    List(String),
    /// The shared object is not named `origin.core`; the value is its name.
    Name(String),
    /// The image has relocations or imports; the value is how many. `origin.core` has nothing
    /// to relocate (L2).
    Relocations(usize),
    /// A writable segment holds bytes that are not the linker's dynamic table: `origin.core`
    /// has no writable data (L2). The value is the segment's RVA.
    WritableData(u64),
    /// The image does not have exactly one read-execute segment; the value is how many.
    CodeSegments(usize),
    /// The image exports a symbol the list does not name.
    Unlisted(String),
    /// A listed entry point is not among the image's exports.
    Missing(String),
    /// An export is not a public function with bytes inside the code, or overlaps another.
    Export(String),
    /// A stub holds a number of `syscall` byte pairs other than one.
    Syscalls {
        /// The stub.
        name: String,
        /// How many pairs its bytes hold.
        found: usize,
    },
    /// The code holds a `syscall` byte pair outside every stub; the value is its offset.
    StraySyscall(u64),
    /// A byte of the code belongs to no stub and is not padding; the value is its offset.
    Stray(u64),
    /// A size or an offset fell outside what the reader validated (a bug here or there).
    Inconsistent(&'static str),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elf(e) => write!(f, "ELF input: {e}"),
            Self::List(why) => write!(f, "the list of exports: {why}"),
            Self::Name(name) => write!(
                f,
                "the shared object is named `{name}`, not `{LIBRARY}` (link it with --soname={LIBRARY})"
            ),
            Self::Relocations(n) => write!(
                f,
                "the image has {n} relocations or imports; `origin.core` has nothing to relocate (ADR-0011 L2)"
            ),
            Self::WritableData(rva) => write!(
                f,
                "the writable segment at {rva:#x} holds more than the linker's dynamic table; `origin.core` has no writable data (ADR-0011 L2)"
            ),
            Self::CodeSegments(n) => write!(
                f,
                "the image has {n} read-execute segments; `origin.core` is one"
            ),
            Self::Unlisted(name) => write!(
                f,
                "the image exports `{name}`, which the generator's list does not name; `origin.core` exports the generated entry points and nothing else (ADR-0011 L5)"
            ),
            Self::Missing(name) => {
                write!(
                    f,
                    "the image does not export `{name}`, which the generator's list names"
                )
            }
            Self::Export(name) => write!(
                f,
                "the export `{name}` is not a public function with its bytes inside the code, apart from every other"
            ),
            Self::Syscalls { name, found } => write!(
                f,
                "the stub `{name}` holds {found} `syscall` byte pairs; each stub holds exactly one (ADR-0005 B2, ADR-0011 L6)"
            ),
            Self::StraySyscall(at) => write!(
                f,
                "a `syscall` byte pair at {at:#x} is outside every stub (ADR-0011 L6)"
            ),
            Self::Stray(at) => write!(
                f,
                "the byte at {at:#x} belongs to no stub and is not padding; `origin.core` holds the generated stubs and nothing else (ADR-0011 L4)"
            ),
            Self::Inconsistent(what) => write!(f, "inconsistent description: {what}"),
        }
    }
}

impl std::error::Error for CoreError {}

impl From<ElfError> for CoreError {
    fn from(e: ElfError) -> Self {
        Self::Elf(e)
    }
}

/// A C symbol or a method name of the list: lower-case letters, digits and `_`, starting with a
/// letter, at most 128 bytes. What is written into the kernel's source is only ever this.
fn is_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() <= 128
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `sha256:` and 64 lower-case hexadecimal digits, as bytes.
fn digest(text: &str) -> Option<[u8; 32]> {
    let hex = text.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(core::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

impl Listing {
    /// Reads the generator's list (`core/<library>.exports`): the format line with the
    /// library's name, the release fingerprint, then one entry point a line as
    /// `<number> <symbol> <method> <major>.<minor>`, in strictly ascending number order, with
    /// no symbol or method twice.
    ///
    /// # Errors
    ///
    /// [`CoreError::List`], with the line and what is wrong with it.
    pub fn parse(text: &str) -> Result<Self, CoreError> {
        let bad = |line: usize, why: &str| CoreError::List(format!("line {line}: {why}"));
        let mut lines = text.lines();
        let library = lines
            .next()
            .and_then(|first| first.strip_prefix("# "))
            .and_then(|first| first.strip_prefix(LIST_FORMAT))
            .and_then(|rest| rest.strip_prefix(' '))
            .filter(|name| !name.is_empty() && !name.contains(' '))
            .ok_or_else(|| bad(1, "not `# oidl-core-exports 1 <library>`"))?
            .to_string();
        let release = lines
            .next()
            .and_then(|second| second.strip_prefix("release "))
            .and_then(digest)
            .ok_or_else(|| bad(2, "not `release sha256:<64 hexadecimal digits>`"))?;
        let mut entries: Vec<Listed> = Vec::new();
        for (line, text) in (3usize..).zip(lines) {
            let mut words = text.split(' ');
            let (Some(number), Some(symbol), Some(method), Some(since), None) = (
                words.next(),
                words.next(),
                words.next(),
                words.next(),
                words.next(),
            ) else {
                return Err(bad(
                    line,
                    "not `<number> <symbol> <method> <major>.<minor>`",
                ));
            };
            let number: u64 = number
                .parse()
                .map_err(|_| bad(line, "the call number is not a number"))?;
            // A call number is loaded as 32 bits by its stub.
            if number > u64::from(u32::MAX) {
                return Err(bad(line, "the call number does not fit 32 bits"));
            }
            if !is_identifier(symbol) || !is_identifier(method) {
                return Err(bad(
                    line,
                    "a symbol or a method that is not a plain identifier",
                ));
            }
            let since = since
                .split_once('.')
                .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
                .ok_or_else(|| bad(line, "the version is not `<major>.<minor>`"))?;
            if entries.last().is_some_and(|last| last.number >= number) {
                return Err(bad(line, "call numbers must ascend"));
            }
            if entries
                .iter()
                .any(|e| e.symbol == symbol || e.method == method)
            {
                return Err(bad(line, "a symbol or a method named twice"));
            }
            entries.push(Listed {
                number,
                symbol: symbol.to_string(),
                method: method.to_string(),
                since,
            });
        }
        if entries.is_empty() {
            return Err(bad(3, "no entry point is listed"));
        }
        Ok(Self {
            library,
            release,
            entries,
        })
    }

    /// The symbols `origin.core` exports, for reading a program that imports from it.
    #[must_use]
    pub fn symbols(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.symbol.as_str()).collect()
    }

    /// Where `symbol` is among the exports sorted by name, which is the order of
    /// [`CoreLib::exports`]: what an import of it is written as.
    #[must_use]
    pub fn export_index(&self, symbol: &str) -> Option<usize> {
        let mut names = self.symbols();
        names.sort_unstable();
        names.iter().position(|name| *name == symbol)
    }
}

/// Where the byte pair `SYSCALL` starts in `bytes`, at every alignment.
fn syscall_pairs(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    bytes
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| **pair == SYSCALL)
        .map(|(at, _)| at)
}

/// Reads `file`, the linker's output for `origin.core`, against `listing`, and lays it out for
/// the kernel.
///
/// # Errors
///
/// Each rule of the module documentation has its own [`CoreError`].
pub fn describe(file: &[u8], listing: &Listing) -> Result<CoreLib, CoreError> {
    let parsed = elf::parse_linked(file, Linking::Library)?;
    match parsed.soname.as_deref() {
        Some(LIBRARY) => {}
        other => return Err(CoreError::Name(other.unwrap_or_default().to_string())),
    }
    let bound = parsed
        .relocations
        .len()
        .saturating_add(parsed.imports.len());
    if bound != 0 {
        return Err(CoreError::Relocations(bound));
    }
    // No writable data: the one thing a writable segment may hold is the dynamic table, which
    // the linker writes for itself and nothing reads at run time.
    for s in &parsed.segments {
        let is_table = parsed.dynamic_table == Some((s.file_offset, s.file_size));
        if s.permissions == Permissions::ReadWrite && s.file_size != 0 && !is_table {
            return Err(CoreError::WritableData(s.vaddr));
        }
    }
    let mut code_segments = parsed
        .segments
        .iter()
        .filter(|s| s.permissions == Permissions::ReadExecute);
    let (text, more) = (code_segments.next(), code_segments.count());
    let Some(text) = text.filter(|_| more == 0) else {
        let found = parsed
            .segments
            .iter()
            .filter(|s| s.permissions == Permissions::ReadExecute)
            .count();
        return Err(CoreError::CodeSegments(found));
    };
    let start =
        usize::try_from(text.file_offset).map_err(|_| CoreError::Inconsistent("a file offset"))?;
    let length =
        usize::try_from(text.file_size).map_err(|_| CoreError::Inconsistent("a file size"))?;
    let code = start
        .checked_add(length)
        .and_then(|end| file.get(start..end))
        .ok_or(CoreError::Inconsistent("the code's bytes"))?;

    // Every export is a listed entry point, and every listed entry point is exported.
    for export in &parsed.exports {
        if !listing.entries.iter().any(|e| e.symbol == export.name) {
            return Err(CoreError::Unlisted(export.name.clone()));
        }
    }
    // Each stub's place in the code, in the list's order, which is number order.
    let mut stubs: Vec<(usize, usize, &Listed)> = Vec::with_capacity(listing.entries.len());
    for entry in &listing.entries {
        let mut named = parsed.exports.iter().filter(|x| x.name == entry.symbol);
        let (Some(export), None) = (named.next(), named.next()) else {
            return Err(CoreError::Missing(entry.symbol.clone()));
        };
        let wrong = || CoreError::Export(export.name.clone());
        if !export.function || !export.public || export.size == 0 {
            return Err(wrong());
        }
        let at = export
            .value
            .checked_sub(text.vaddr)
            .and_then(|at| usize::try_from(at).ok())
            .ok_or_else(wrong)?;
        let end = usize::try_from(export.size)
            .ok()
            .and_then(|size| at.checked_add(size))
            .filter(|&end| end <= code.len())
            .ok_or_else(wrong)?;
        stubs.push((at, end, entry));
    }
    // No two stubs share a byte.
    let mut places: Vec<(usize, usize, &str)> = stubs
        .iter()
        .map(|&(at, end, entry)| (at, end, entry.symbol.as_str()))
        .collect();
    places.sort_unstable();
    for pair in places.windows(2) {
        if let [(_, end, _), (next, _, name)] = pair
            && next < end
        {
            return Err(CoreError::Export((*name).to_string()));
        }
    }

    // One `syscall` in each stub, and that is all of them.
    let mut pins = Vec::with_capacity(stubs.len());
    for &(at, end, entry) in &stubs {
        let body = code
            .get(at..end)
            .ok_or(CoreError::Inconsistent("a stub's bytes"))?;
        let mut found = syscall_pairs(body);
        let (Some(pair), None) = (found.next(), found.next()) else {
            return Err(CoreError::Syscalls {
                name: entry.symbol.clone(),
                found: syscall_pairs(body).count(),
            });
        };
        pins.push(CorePin {
            method: entry.method.clone(),
            number: entry.number,
            offset: at.saturating_add(pair) as u64,
        });
    }
    for pair in syscall_pairs(code) {
        let inside = stubs
            .iter()
            .any(|&(at, end, _)| pair >= at && pair.saturating_add(SYSCALL.len()) <= end);
        if !inside {
            return Err(CoreError::StraySyscall(pair as u64));
        }
    }
    // And nothing else: a byte outside every stub is the linker's padding.
    for (offset, &byte) in code.iter().enumerate() {
        let in_stub = places
            .iter()
            .any(|&(at, end, _)| offset >= at && offset < end);
        if !in_stub && byte != PADDING {
            return Err(CoreError::Stray(offset as u64));
        }
    }

    let mut exports: Vec<CoreExport> = stubs
        .iter()
        .map(|&(at, _, entry)| CoreExport {
            name: entry.symbol.clone(),
            offset: at as u64,
            since: entry.since,
        })
        .collect();
    exports.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(CoreLib {
        span: text.mapped_size,
        code: code.to_vec(),
        exports,
        pins,
        release: listing.release,
        source_digest: Sha256::digest(file),
    })
}

/// One file of a description: its name inside the output directory, and its bytes.
pub type File = (String, Vec<u8>);

/// The first line of the description [`CoreLib::rust`] writes.
#[must_use]
pub fn generated_line() -> String {
    format!(
        "// @generated by `onbf-pack corelib` from the linker output for `{LIBRARY}`. Do not edit.\n"
    )
}

impl CoreLib {
    /// The kernel-private description: `origin_core.rs`, then `origin_core.0.bin`, the code.
    /// The Rust file defines `pub(crate) static ORIGIN_CORE: CoreImage`, with the kernel's
    /// `CoreImage` and `CoreExport` and its generated `Pins` in scope where it is included, and
    /// finds the code in the directory the environment variable `variable` names when the
    /// kernel is compiled.
    ///
    /// # Errors
    ///
    /// [`CoreError::List`] if `variable` is not an upper-case environment variable name.
    pub fn rust(&self, variable: &str) -> Result<Vec<File>, CoreError> {
        let plain = variable.len() <= 64
            && variable
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_uppercase())
            && variable
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        if !plain {
            return Err(CoreError::List(format!(
                "`{variable}` is not an upper-case environment variable name of at most 64 bytes"
            )));
        }
        let file = format!("{NAME}.0.bin");
        let mut text = generated_line();
        text.push_str(&format!(
            "// `{LIBRARY}`, which the kernel carries in its image and maps into every process\n\
             // that is pinned per call (ADR-0011 Amendment 2, L2 to L7).\n\
             // The linker output's SHA-256: {}.\n\
             \n\
             /// `{LIBRARY}`, as the build laid it out.\n\
             pub(crate) static {}: CoreImage = CoreImage {{\n    \
             span: {:#x},\n    \
             code: include_bytes!(concat!(env!(\"{variable}\"), \"/{file}\")),\n    \
             exports: &[\n",
            hex(&self.source_digest),
            NAME.to_ascii_uppercase(),
            self.span,
        ));
        for export in &self.exports {
            text.push_str(&format!(
                "        CoreExport {{ name: \"{}\", offset: {:#x}, since: ({}, {}) }},\n",
                export.name, export.offset, export.since.0, export.since.1
            ));
        }
        text.push_str("    ],\n    pins: Pins {\n");
        for pin in &self.pins {
            text.push_str(&format!("        {}: {:#x},\n", pin.method, pin.offset));
        }
        text.push_str("    },\n    release: [\n");
        for row in self.release.chunks(8) {
            let bytes: Vec<String> = row.iter().map(|b| format!("{b:#04x},")).collect();
            text.push_str(&format!("        {}\n", bytes.join(" ")));
        }
        text.push_str("    ],\n};\n");
        Ok(vec![
            (format!("{NAME}.rs"), text.into_bytes()),
            (file, self.code.clone()),
        ])
    }
}
