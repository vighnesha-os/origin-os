//! A program the kernel carries in its own image, described for it at build time (ADR-0011
//! Amendment 2, L1; the note to ONBF §8.3; task S4-1). No ONBF profile: until the Loader Service
//! exists (milestone M6), the kernel's launch path reads no image at run time, so the build lays
//! each program out here and the kernel compiles the result in.
//!
//! [`describe`] reads `rust-lld`'s output with the same reader as the KRN conversion
//! ([`crate::elf::parse`]), which refuses each of ONBF §6.1.2's hard errors, and adds the
//! program's own rule: exactly one read-execute segment. Every relocation is base-relative, and
//! the program imports nothing.
//!
//! [`describe_importing`] reads a program linked against `origin.core` (task S4-2; ADR-0011
//! Amendment 2, L3): its one `DT_NEEDED` names that library, each symbol it leaves undefined is
//! one of the library's exports, and each `R_X86_64_GLOB_DAT` relocation against one becomes an
//! import: a slot, and which export goes there. The launch path binds every import before the
//! program's first thread exists, on a page that is read-only to the program from the start,
//! so an import slot must lie in a read-only-after-relocation segment; one anywhere else is
//! refused here. The description names an export by its place among the library's exports in
//! name order, which is the order `onbf-pack corelib` writes them in, and carries the release
//! fingerprint of the list it was bound against, so the kernel can refuse a program bound to
//! another `origin.core` than the one it carries. The program's own bytes hold no call number
//! and no offset of `origin.core`.
//!
//! [`Carried::rust`] writes the kernel-private description: one Rust file holding a static of
//! the kernel's `program::Carried` type, and one file of bytes for each segment that has any.
//! Each segment's bytes are its file-backed bytes with every `DT_RELA` addend written into its
//! slot, so that the launch path's only work on a slot is to add the base. The same ELF always
//! gives the same files, byte for byte (§6.1.2's determinism test; CLAUDE.md §75): the
//! conversion adds nothing from the environment, a clock or a path. What the compiler put in
//! the program's bytes is carried as it is: a panic location's file name held the path of the
//! toolchain's `core` source under the building account's home directory until TASKS item 32's
//! remapping reached the ring-3 build (S4-1's review, finding F5).
//!
//! [`stale`] keeps the output directory safe: the files it removes are only this tool's own,
//! from an earlier description of the same program (finding F9). [`update`] says, from what the
//! caller read of the directory, which files to write and which to remove, so that the caller
//! decides nothing about it: it reads and writes the directory, and this module touches no file
//! (ADR-0011 Amendment 4, T1).

use core::fmt;
use std::collections::BTreeMap;

use origin_crypto::Sha256;

use crate::corelib::{self, Listing};
use crate::elf::{self, ElfError, Linking, Permissions};
use crate::sign::hex;

/// How the kernel maps a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read-only.
    Read,
    /// Read and execute.
    ReadExecute,
    /// Read and write, never executable.
    ReadWrite,
    /// Read-only to the program: written, with its relocations, by the launch path before the
    /// first thread exists (RELRO, ONBF §6.1).
    ReadAfterRelocation,
}

impl Access {
    /// The variant's name in the kernel's `program::SegmentAccess`.
    const fn kernel_name(self) -> &'static str {
        match self {
            Self::Read => "Read",
            Self::ReadExecute => "ReadExecute",
            Self::ReadWrite => "ReadWrite",
            Self::ReadAfterRelocation => "ReadAfterRelocation",
        }
    }
}

/// One segment as the kernel maps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// Its offset from the program's base, a multiple of 4096.
    pub rva: u64,
    /// The bytes it maps, a whole number of pages.
    pub size: u64,
    /// How it is mapped.
    pub access: Access,
    /// Its file-backed bytes, with every `DT_RELA` addend in its slot. The rest of `size` is
    /// zero.
    pub bytes: Vec<u8>,
}

/// One import of a program: a slot the launch path writes an export's address into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedImport {
    /// The slot's offset from the base: 8-byte aligned, in the file-backed bytes of a
    /// read-only-after-relocation segment, and no relocation slot.
    pub slot: u64,
    /// Which export of `origin.core` goes there: its place among the exports in name order.
    pub export: usize,
    /// The export's name, for the description's comments.
    pub symbol: String,
}

/// A program, laid out for the kernel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Carried {
    /// The entry point's offset from the base, inside the read-execute segment.
    pub entry: u64,
    /// The bytes from the base to the end of the last segment, a whole number of pages.
    pub span: u64,
    /// The segments, in ascending, non-overlapping order.
    pub segments: Vec<Segment>,
    /// Each relocation slot's offset from the base, strictly ascending: the launch path adds the
    /// base to the 8 bytes there. Each lies in the file-backed bytes of a writable or RELRO
    /// segment and is 8-byte aligned.
    pub relocations: Vec<u64>,
    /// Each import, strictly ascending by slot. Empty for a program that imports nothing.
    pub imports: Vec<CarriedImport>,
    /// The release fingerprint of the `origin.core` the imports were bound against (the
    /// generator's list of its exports). `None` for a program read with no library.
    pub core: Option<[u8; 32]>,
    /// SHA-256 of the linker output it was made from, for the record.
    pub source_digest: [u8; 32],
}

/// Why a program could not be described.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CarriedError {
    /// The linker output breaks the conversion contract (ONBF §6.1.2).
    Elf(ElfError),
    /// The program does not have exactly one read-execute segment; the value is how many.
    CodeSegments(usize),
    /// An import slot is not in a read-only-after-relocation segment: the program could write
    /// over the address it calls through (ONBF §6.1: full RELRO). The value is the slot.
    ImportSlot(u64),
    /// A segment's bytes or a relocation slot fell outside what the reader validated (a bug in
    /// this module, or in the reader).
    Inconsistent(&'static str),
    /// The name is not a Rust identifier in lower case (`[a-z][a-z0-9_]*`, at most 64 bytes).
    Name(String),
    /// The environment variable is not `[A-Z][A-Z0-9_]*`, at most 64 bytes.
    Variable(String),
    /// The output directory holds a file of a description's names that this tool did not write
    /// for this program: [`stale`] refuses to replace or remove it.
    Foreign(String),
    /// A name [`update`] would have its caller write or remove is not a plain file name: it is
    /// empty, `.` or `..`, or holds `/`, `\` or a NUL byte.
    FileName(String),
}

impl fmt::Display for CarriedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elf(e) => write!(f, "ELF input: {e}"),
            Self::CodeSegments(n) => write!(
                f,
                "the program has {n} read-execute segments; a carried program has exactly one"
            ),
            Self::ImportSlot(slot) => write!(
                f,
                "the import slot at {slot:#x} is not in a read-only-after-relocation segment; link with -z relro -z now (ONBF §6.1)"
            ),
            Self::Inconsistent(what) => write!(f, "inconsistent description: {what}"),
            Self::Name(name) => write!(
                f,
                "`{name}` is not a lower-case Rust identifier of at most 64 bytes"
            ),
            Self::Variable(name) => write!(
                f,
                "`{name}` is not an upper-case environment variable name of at most 64 bytes"
            ),
            Self::Foreign(file) => write!(
                f,
                "{file} is not part of a description `onbf-pack carried` wrote for this program: nothing replaced or removed (is --out right?)"
            ),
            Self::FileName(file) => write!(
                f,
                "{file:?} is not a plain file name: nothing written or removed"
            ),
        }
    }
}

impl std::error::Error for CarriedError {}

impl From<ElfError> for CarriedError {
    fn from(e: ElfError) -> Self {
        Self::Elf(e)
    }
}

/// Reads `file`, a program's `rust-lld` output, and lays it out for the kernel: with
/// [`describe_importing`] when it was linked against `origin.core`, whose exports `listing`
/// names, and with [`describe`] when `listing` is `None`.
///
/// # Errors
///
/// As [`describe`] or [`describe_importing`].
pub fn describe_program(file: &[u8], listing: Option<&Listing>) -> Result<Carried, CarriedError> {
    match listing {
        None => describe(file),
        Some(listing) => describe_importing(file, listing),
    }
}

/// Reads `file`, the `rust-lld` output of a program that imports nothing, and lays it out for
/// the kernel.
pub fn describe(file: &[u8]) -> Result<Carried, CarriedError> {
    layout(file, elf::parse(file)?, None)
}

/// Reads `file`, the `rust-lld` output of a program linked against `origin.core`, whose exports
/// `listing` names, and lays it out for the kernel with its imports.
pub fn describe_importing(file: &[u8], listing: &Listing) -> Result<Carried, CarriedError> {
    let exports = listing.symbols();
    let parsed = elf::parse_linked(
        file,
        Linking::Importing {
            library: corelib::LIBRARY,
            exports: &exports,
        },
    )?;
    layout(file, parsed, Some(listing))
}

/// Lays `parsed`, the reading of `file`, out for the kernel; its imports are named by their
/// place in `listing`.
fn layout(
    file: &[u8],
    parsed: elf::Elf,
    listing: Option<&Listing>,
) -> Result<Carried, CarriedError> {
    let code = parsed
        .segments
        .iter()
        .filter(|s| s.permissions == Permissions::ReadExecute)
        .count();
    if code != 1 {
        return Err(CarriedError::CodeSegments(code));
    }
    let mut segments = Vec::with_capacity(parsed.segments.len());
    for s in &parsed.segments {
        let start = usize::try_from(s.file_offset)
            .map_err(|_| CarriedError::Inconsistent("a file offset"))?;
        let length =
            usize::try_from(s.file_size).map_err(|_| CarriedError::Inconsistent("a file size"))?;
        let bytes = start
            .checked_add(length)
            .and_then(|end| file.get(start..end))
            .ok_or(CarriedError::Inconsistent("a segment's bytes"))?;
        let access = match (s.permissions, s.relro) {
            (Permissions::Read, _) => Access::Read,
            (Permissions::ReadExecute, _) => Access::ReadExecute,
            (Permissions::ReadWrite, false) => Access::ReadWrite,
            (Permissions::ReadWrite, true) => Access::ReadAfterRelocation,
        };
        segments.push(Segment {
            rva: s.vaddr,
            size: s.mapped_size,
            access,
            bytes: bytes.to_vec(),
        });
    }
    let mut relocations = Vec::with_capacity(parsed.relocations.len());
    for r in &parsed.relocations {
        if let Some(addend) = r.addend {
            place_addend(&mut segments, r.slot, addend)?;
        }
        relocations.push(r.slot);
    }
    let span = segments
        .last()
        .and_then(|s| s.rva.checked_add(s.size))
        .ok_or(CarriedError::Inconsistent("the span"))?;
    let mut imports = Vec::with_capacity(parsed.imports.len());
    for import in &parsed.imports {
        // The reader held the slot to a writable segment's file-backed bytes; the launch path
        // needs more: a page the program can never write.
        let relro = segments.iter().any(|s| {
            s.access == Access::ReadAfterRelocation
                && import
                    .slot
                    .checked_sub(s.rva)
                    .and_then(|at| at.checked_add(8))
                    .is_some_and(|end| end <= s.bytes.len() as u64)
        });
        if !relro {
            return Err(CarriedError::ImportSlot(import.slot));
        }
        let export = listing
            .and_then(|listing| listing.export_index(&import.symbol))
            .ok_or(CarriedError::Inconsistent("an import of no listed export"))?;
        // Whatever the linker left in the slot, the description holds zero there: its value
        // is the launch path's to write.
        place_addend(&mut segments, import.slot, 0)?;
        imports.push(CarriedImport {
            slot: import.slot,
            export,
            symbol: import.symbol.clone(),
        });
    }
    Ok(Carried {
        entry: parsed.entry,
        span,
        segments,
        relocations,
        imports,
        core: listing.map(|listing| listing.release),
        source_digest: Sha256::digest(file),
    })
}

/// Writes `addend` into the 8 bytes at `slot`, which the reader found in the file-backed bytes
/// of a writable segment.
fn place_addend(segments: &mut [Segment], slot: u64, addend: u64) -> Result<(), CarriedError> {
    let bad = CarriedError::Inconsistent("a relocation slot");
    let segment = segments
        .iter_mut()
        .find(|s| slot.checked_sub(s.rva).is_some_and(|d| d < s.size))
        .ok_or_else(|| bad.clone())?;
    if !matches!(
        segment.access,
        Access::ReadWrite | Access::ReadAfterRelocation
    ) {
        return Err(bad);
    }
    let at = slot
        .checked_sub(segment.rva)
        .and_then(|d| usize::try_from(d).ok())
        .ok_or_else(|| bad.clone())?;
    let end = at.checked_add(8).ok_or_else(|| bad.clone())?;
    segment
        .bytes
        .get_mut(at..end)
        .ok_or(bad)?
        .copy_from_slice(&addend.to_le_bytes());
    Ok(())
}

/// Relocation offsets per line of the generated table.
const PER_LINE: usize = 6;

/// One file of a description: its name inside the output directory, and its bytes.
pub type File = (String, Vec<u8>);

/// The first line of every description [`Carried::rust`] writes for `name`.
#[must_use]
pub fn generated_line(name: &str) -> String {
    format!(
        "// @generated by `onbf-pack carried` from the linker output for `{name}`. Do not edit.\n"
    )
}

/// Whether `file` has the name of a file [`Carried::rust`] writes for `name`: `<name>.rs`, or
/// `<name>.<k>.bin` for a decimal `k`.
fn is_output(name: &str, file: &str) -> bool {
    let Some(rest) = file
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('.'))
    else {
        return false;
    };
    rest == "rs"
        || rest
            .strip_suffix(".bin")
            .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
}

/// What may be removed from an output directory before a description of `name`, `files`, is
/// written there (S4-1's review, finding F9). `present` is every file name in the directory, and
/// `description` the bytes of its `<name>.rs`, if it has one. `first_line` is the line the
/// describing command writes first for `name`: [`generated_line`] for a program, and
/// `corelib::generated_line` for `origin.core`.
///
/// A directory that holds any file of a description's names for `name` must hold a `<name>.rs`
/// that begins with that line. Otherwise the directory is not one
/// this tool wrote to, and [`CarriedError::Foreign`] names the file: nothing may be written or
/// removed. In a directory that is one, the stale files are the `<name>.<k>.bin` that `files`
/// no longer holds, and nothing else: a wrong `--out` cannot lose a file.
///
/// # Errors
///
/// [`CarriedError::Foreign`], as above.
pub fn stale(
    name: &str,
    first_line: &str,
    files: &[File],
    present: &[String],
    description: Option<&[u8]>,
) -> Result<Vec<String>, CarriedError> {
    let mut ours: Vec<&String> = present.iter().filter(|f| is_output(name, f)).collect();
    ours.sort();
    let Some(&first) = ours.first() else {
        return Ok(Vec::new());
    };
    if !description.is_some_and(|d| d.starts_with(first_line.as_bytes())) {
        let rs = format!("{name}.rs");
        return Err(CarriedError::Foreign(
            ours.iter()
                .find(|f| ***f == rs)
                .copied()
                .unwrap_or(first)
                .clone(),
        ));
    }
    Ok(ours
        .into_iter()
        .filter(|f| f.ends_with(".bin") && !files.iter().any(|(written, _)| written == *f))
        .cloned()
        .collect())
}

/// What a caller does to an output directory so that it holds `files`, a description of
/// `name`, and nothing stale of an earlier one ([`update`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update<'a> {
    /// The files whose bytes the directory does not hold yet, in the order of `files`: each is
    /// written under its name, replacing what is there.
    pub write: Vec<&'a File>,
    /// The stale files, by name: each is removed.
    pub remove: Vec<String>,
}

/// Whether `file` names one file inside a directory and nothing else.
fn is_plain_file_name(file: &str) -> bool {
    !file.is_empty()
        && file != "."
        && file != ".."
        && !file.bytes().any(|b| matches!(b, b'/' | b'\\' | 0))
}

/// What to write and remove in an output directory before it holds `files`, a description of
/// `name` whose first line is `first_line` (as for [`stale`]). `present` is what the caller read
/// of the directory: every regular file in it whose name is UTF-8, with its bytes. A file whose
/// bytes are already there is not written again, so a build that changed nothing leaves the
/// kernel's inputs as they were. Every name returned is a plain file name, checked here, so the
/// caller writes and removes what it is given without judging it.
///
/// # Errors
///
/// [`CarriedError::Foreign`], as [`stale`] gives it: nothing may be written or removed.
/// [`CarriedError::FileName`] if a name in `files` or a stale one is not a plain file name.
pub fn update<'a>(
    name: &str,
    first_line: &str,
    files: &'a [File],
    present: &BTreeMap<String, Vec<u8>>,
) -> Result<Update<'a>, CarriedError> {
    let names: Vec<String> = present.keys().cloned().collect();
    let description = present.get(&format!("{name}.rs")).map(Vec::as_slice);
    let remove = stale(name, first_line, files, &names, description)?;
    let write: Vec<&File> = files
        .iter()
        .filter(|(file, bytes)| present.get(file) != Some(bytes))
        .collect();
    if let Some(bad) = files
        .iter()
        .map(|(file, _)| file)
        .chain(&remove)
        .find(|file| !is_plain_file_name(file))
    {
        return Err(CarriedError::FileName(bad.clone()));
    }
    Ok(Update { write, remove })
}

fn is_name(name: &str, first: fn(u8) -> bool, rest: fn(u8) -> bool) -> bool {
    let bytes = name.as_bytes();
    bytes.len() <= 64
        && bytes.first().is_some_and(|&b| first(b))
        && bytes.iter().all(|&b| rest(b) || b == b'_')
}

impl Carried {
    /// The kernel-private description of this program: `{name}.rs`, then one `{name}.{i}.bin`
    /// for each segment `i` with bytes. The Rust file defines `pub(crate) static {NAME}:
    /// Carried`, with the kernel's `Carried`, `Segment`, `SegmentAccess` and `Import` in scope
    /// where it is included, and finds the byte files in the directory the environment variable
    /// `variable` names when the kernel is compiled.
    pub fn rust(&self, name: &str, variable: &str) -> Result<Vec<File>, CarriedError> {
        if !is_name(
            name,
            |b| b.is_ascii_lowercase(),
            |b| b.is_ascii_lowercase() || b.is_ascii_digit(),
        ) {
            return Err(CarriedError::Name(name.to_string()));
        }
        if !is_name(
            variable,
            |b| b.is_ascii_uppercase(),
            |b| b.is_ascii_uppercase() || b.is_ascii_digit(),
        ) {
            return Err(CarriedError::Variable(variable.to_string()));
        }
        let mut files = Vec::new();
        let mut text = generated_line(name);
        text.push_str(&format!(
            "// A program the kernel carries in its image (ADR-0011 Amendment 2, L1).\n\
             // The linker output's SHA-256: {}.\n\
             \n\
             /// `{name}`, as the build laid it out.\n\
             pub(crate) static {}: Carried = Carried {{\n    \
             name: \"{name}\",\n    \
             entry: {:#x},\n    \
             span: {:#x},\n    \
             segments: &[\n",
            hex(&self.source_digest),
            name.to_ascii_uppercase(),
            self.entry,
            self.span,
        ));
        for (index, segment) in self.segments.iter().enumerate() {
            let bytes = if segment.bytes.is_empty() {
                "&[]".to_string()
            } else {
                let file = format!("{name}.{index}.bin");
                let include = format!("include_bytes!(concat!(env!(\"{variable}\"), \"/{file}\"))");
                files.push((file, segment.bytes.clone()));
                include
            };
            text.push_str(&format!(
                "        Segment {{\n            \
                 rva: {:#x},\n            \
                 size: {:#x},\n            \
                 access: SegmentAccess::{},\n            \
                 bytes: {bytes},\n        \
                 }},\n",
                segment.rva,
                segment.size,
                segment.access.kernel_name(),
            ));
        }
        text.push_str("    ],\n    relocations: &[\n");
        for line in self.relocations.chunks(PER_LINE) {
            let offsets: Vec<String> = line.iter().map(|r| format!("{r:#x},")).collect();
            text.push_str(&format!("        {}\n", offsets.join(" ")));
        }
        text.push_str("    ],\n    imports: &[\n");
        for import in &self.imports {
            text.push_str(&format!(
                "        Import {{ slot: {:#x}, export: {}, symbol: \"{}\" }},\n",
                import.slot, import.export, import.symbol
            ));
        }
        text.push_str("    ],\n");
        match &self.core {
            None => text.push_str("    core: None,\n"),
            Some(release) => {
                text.push_str("    core: Some([\n");
                for row in release.chunks(8) {
                    let bytes: Vec<String> = row.iter().map(|b| format!("{b:#04x},")).collect();
                    text.push_str(&format!("        {}\n", bytes.join(" ")));
                }
                text.push_str("    ]),\n");
            }
        }
        text.push_str("};\n");
        files.insert(0, (format!("{name}.rs"), text.into_bytes()));
        Ok(files)
    }
}
