//! A program the kernel carries in its own image, described for it at build time (ADR-0011
//! Amendment 2, L1; the note to ONBF §8.3; task S4-1). No ONBF profile: until the Loader Service
//! exists (milestone M6), the kernel's launch path reads no image at run time, so the build lays
//! each program out here and the kernel compiles the result in.
//!
//! [`describe`] reads `rust-lld`'s output with the same reader as the KRN conversion
//! ([`crate::elf::parse`]), which refuses each of ONBF §6.1.2's hard errors, and adds the
//! program's own rule: exactly one read-execute segment, the range ADR-0005 A6 pins its system
//! calls to. Every relocation is base-relative; `R_X86_64_GLOB_DAT` and `R_X86_64_JUMP_SLOT`
//! are refused too, since nothing is bound until `origin.core` exists (task S4-2, L3).
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
//! from an earlier description of the same program (finding F9).

use core::fmt;

use origin_crypto::Sha256;

use crate::elf::{self, ElfError, Permissions};
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
}

impl fmt::Display for CarriedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Elf(e) => write!(f, "ELF input: {e}"),
            Self::CodeSegments(n) => write!(
                f,
                "the program has {n} read-execute segments; the launch path pins its system calls to exactly one (ADR-0005 A6)"
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
        }
    }
}

impl std::error::Error for CarriedError {}

impl From<ElfError> for CarriedError {
    fn from(e: ElfError) -> Self {
        Self::Elf(e)
    }
}

/// Reads `file`, a program's `rust-lld` output, and lays it out for the kernel.
pub fn describe(file: &[u8]) -> Result<Carried, CarriedError> {
    let parsed = elf::parse(file)?;
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
    Ok(Carried {
        entry: parsed.entry,
        span,
        segments,
        relocations,
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

fn is_name(name: &str, first: fn(u8) -> bool, rest: fn(u8) -> bool) -> bool {
    let bytes = name.as_bytes();
    bytes.len() <= 64
        && bytes.first().is_some_and(|&b| first(b))
        && bytes.iter().all(|&b| rest(b) || b == b'_')
}

impl Carried {
    /// The kernel-private description of this program: `{name}.rs`, then one `{name}.{i}.bin`
    /// for each segment `i` with bytes. The Rust file defines `pub(crate) static {NAME}:
    /// Carried`, with the kernel's `Carried`, `Segment` and `SegmentAccess` in scope where it
    /// is included, and finds the byte files in the directory the environment variable
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
        text.push_str("    ],\n};\n");
        files.insert(0, (format!("{name}.rs"), text.into_bytes()));
        Ok(files)
    }
}
