//! ELF64 input, read against the KRN conversion contract (ONBF §6.1.2, ONBF-R11).
//!
//! Sources: the System V ABI generic part (ELF gABI: `e_ident`, the ELF header, program
//! headers, the dynamic section, `Elf64_Rela`, and the `SHT_RELR` encoding) and the x86-64
//! psABI (`EM_X86_64`, `R_X86_64_RELATIVE`). Every field is decoded explicitly from bytes,
//! with no struct casts (ONBF-R7).
//!
//! The parser accepts exactly the shape that `rust-lld` writes for the Framekernel, a
//! static PIE linked with `--no-dynamic-linker -z text -z separate-loadable-segments
//! -z max-page-size=4096 -z common-page-size=4096 -z norelro -z noexecstack`, and rejects
//! everything else with a typed [`ElfError`]. It returns validated data only: no partial
//! results.
//!
//! Program headers: `PT_LOAD`; `PT_DYNAMIC` and `PT_GNU_STACK` (non-executable), at most
//! once each; `PT_NOTE`, `PT_PHDR` and `PT_GNU_EH_FRAME`, which are ignored. Anything else
//! is a hard error.

use core::fmt;

use crate::relr::{self, RelrError};

/// Page size of every Origin mapping, and the only accepted `p_align`.
pub const PAGE_SIZE: u64 = 4096;
const PAGE_MASK: u64 = PAGE_SIZE - 1;
/// Largest image span: the page-rounded end of the highest segment (ONBF §6.1, errata G4).
pub const MAX_IMAGE_SPAN: u64 = 1 << 31;

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const EI_NIDENT: usize = 16;
const ELFCLASS32: u8 = 1;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ELFDATA2MSB: u8 = 2;
const EV_CURRENT: u32 = 1;
const ET_DYN: u16 = 3;
const EM_386: u16 = 3;
const EM_X86_64: u16 = 62;
const EHDR_SIZE: u16 = 64;
const PHDR_SIZE: u16 = 56;
const PHDR_BYTES: usize = PHDR_SIZE as usize;
/// `e_phnum` value meaning "the real count is in section header 0" (gABI `PN_XNUM`).
const PN_XNUM: u16 = 0xFFFF;

/// Program header types (gABI, and the GNU extensions `rust-lld` emits).
pub mod pt {
    /// Loadable segment.
    pub const LOAD: u32 = 1;
    /// Dynamic linking information.
    pub const DYNAMIC: u32 = 2;
    /// Program interpreter path.
    pub const INTERP: u32 = 3;
    /// Auxiliary information.
    pub const NOTE: u32 = 4;
    /// The program header table itself.
    pub const PHDR: u32 = 6;
    /// Thread-local storage template.
    pub const TLS: u32 = 7;
    /// `.eh_frame_hdr` location (GNU).
    pub const GNU_EH_FRAME: u32 = 0x6474_E550;
    /// Stack permissions (GNU).
    pub const GNU_STACK: u32 = 0x6474_E551;
    /// Read-only-after-relocation range (GNU).
    pub const GNU_RELRO: u32 = 0x6474_E552;
}

/// Segment permission bits (`p_flags`).
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Dynamic tags (gABI, plus the GNU tags `rust-lld` emits).
pub mod dt {
    /// End of the dynamic array.
    pub const NULL: u64 = 0;
    /// Name of a needed library.
    pub const NEEDED: u64 = 1;
    /// Symbol hash table.
    pub const HASH: u64 = 4;
    /// Dynamic string table.
    pub const STRTAB: u64 = 5;
    /// Dynamic symbol table.
    pub const SYMTAB: u64 = 6;
    /// `Elf64_Rela` table.
    pub const RELA: u64 = 7;
    /// Size of the `Elf64_Rela` table in bytes.
    pub const RELASZ: u64 = 8;
    /// Size of one `Elf64_Rela` entry.
    pub const RELAENT: u64 = 9;
    /// Size of the string table.
    pub const STRSZ: u64 = 10;
    /// Size of one symbol table entry.
    pub const SYMENT: u64 = 11;
    /// Initialization function.
    pub const INIT: u64 = 12;
    /// Termination function.
    pub const FINI: u64 = 13;
    /// Debugger hook.
    pub const DEBUG: u64 = 21;
    /// Text relocations are present.
    pub const TEXTREL: u64 = 22;
    /// Initialization function array.
    pub const INIT_ARRAY: u64 = 25;
    /// Termination function array.
    pub const FINI_ARRAY: u64 = 26;
    /// Size of `DT_INIT_ARRAY`.
    pub const INIT_ARRAYSZ: u64 = 27;
    /// Size of `DT_FINI_ARRAY`.
    pub const FINI_ARRAYSZ: u64 = 28;
    /// Flag bits (`DF_*`).
    pub const FLAGS: u64 = 30;
    /// Pre-initialization function array.
    pub const PREINIT_ARRAY: u64 = 32;
    /// Size of `DT_PREINIT_ARRAY`.
    pub const PREINIT_ARRAYSZ: u64 = 33;
    /// Size of the RELR table in bytes.
    pub const RELRSZ: u64 = 35;
    /// RELR table.
    pub const RELR: u64 = 36;
    /// Size of one RELR entry.
    pub const RELRENT: u64 = 37;
    /// GNU-style symbol hash table.
    pub const GNU_HASH: u64 = 0x6FFF_FEF5;
    /// Number of leading relative relocations in `DT_RELA`.
    pub const RELACOUNT: u64 = 0x6FFF_FFF9;
    /// Flag bits (`DF_1_*`).
    pub const FLAGS_1: u64 = 0x6FFF_FFFB;
}

const DYN_SIZE: usize = 16;
const RELA_SIZE: u64 = 24;
const RELA_BYTES: usize = 24;
const RELR_SIZE: u64 = 8;
/// `B + A`: the only relocation type a KRN image may carry (x86-64 psABI).
pub const R_X86_64_RELATIVE: u32 = 8;

/// The permissions of a loadable segment. Exactly these three are accepted (ONBF §6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permissions {
    /// `PF_R`.
    Read,
    /// `PF_R | PF_X`.
    ReadExecute,
    /// `PF_R | PF_W`.
    ReadWrite,
}

/// A validated `PT_LOAD` segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoadSegment {
    /// Index of the program header.
    pub header: u16,
    /// Permissions.
    pub permissions: Permissions,
    /// RVA (`p_vaddr`), a multiple of 4096.
    pub vaddr: u64,
    /// File offset (`p_offset`), a multiple of 4096.
    pub file_offset: u64,
    /// File-backed bytes (`p_filesz`), all inside the input.
    pub file_size: u64,
    /// `p_memsz`, rounded up to a whole number of pages (errata G4).
    pub mapped_size: u64,
}

impl LoadSegment {
    /// True if `[rva, rva + len)` lies inside this segment's file-backed bytes.
    fn file_backed(&self, rva: u64, len: u64) -> bool {
        let end = self.vaddr.saturating_add(self.file_size);
        rva >= self.vaddr && rva.checked_add(len).is_some_and(|e| e <= end)
    }

    /// True if `rva` lies inside this segment's page-rounded memory range.
    fn maps(&self, rva: u64) -> bool {
        rva >= self.vaddr && rva < self.vaddr.saturating_add(self.mapped_size)
    }
}

/// One base-relative relocation slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// RVA of the 8-byte slot; 8-byte aligned, in the file-backed bytes of an RW segment.
    pub slot: u64,
    /// The explicit addend from `DT_RELA`, which the packer writes into the slot. `None`
    /// for a `DT_RELR` slot, whose addend is already in place.
    pub addend: Option<u64>,
}

/// A validated input image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elf {
    /// `e_entry`, inside the file-backed bytes of an RX segment.
    pub entry: u64,
    /// Loadable segments, in ascending, non-overlapping page-rounded RVA order.
    pub segments: Vec<LoadSegment>,
    /// Relocation slots in strictly ascending order, from `DT_RELA` and `DT_RELR` merged.
    pub relocations: Vec<Relocation>,
}

impl Elf {
    /// Index of the segment whose page-rounded memory range contains `rva`.
    pub fn segment_containing(&self, rva: u64) -> Option<usize> {
        self.segments.iter().position(|s| s.maps(rva))
    }
}

/// Why a relocation slot is not acceptable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotProblem {
    /// The slot is not 8-byte aligned.
    Misaligned,
    /// The slot is not inside any `PT_LOAD`.
    Unmapped,
    /// The slot is in a segment that is not RW: a text or read-only-data relocation.
    NotWritable(Permissions),
    /// The slot is in the zero-filled tail (bss) of its segment, where no addend can be
    /// stored.
    Bss,
}

/// Why an ELF input was rejected. Each variant names the rule it enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// The input ends inside the named structure.
    Truncated(&'static str),
    /// The input does not start with the ELF magic.
    NotElf,
    /// `ELFCLASS32` (ONBF-R11). x86-64 and IA-32 have their own variants.
    Elf32 {
        /// `e_machine`.
        machine: u16,
    },
    /// An x32 (ILP32 x86-64) object: `ELFCLASS32` with `EM_X86_64` (ONBF-R11).
    X32,
    /// `EM_386`, IA-32 code (ONBF-R11).
    I386,
    /// `EI_CLASS` is neither 32- nor 64-bit.
    UnsupportedClass(u8),
    /// `EI_DATA` is not `ELFDATA2LSB`.
    UnsupportedDataEncoding(u8),
    /// `EI_VERSION` or `e_version` is not `EV_CURRENT`.
    UnsupportedVersion(u32),
    /// `e_type` is not `ET_DYN`: the input must be a PIE.
    NotPie(u16),
    /// `e_machine` is not `EM_X86_64`.
    UnsupportedMachine(u16),
    /// `e_ehsize` is not 64 or `e_phentsize` is not 56.
    HeaderSize {
        /// `e_ehsize`.
        ehsize: u16,
        /// `e_phentsize`.
        phentsize: u16,
    },
    /// `e_phnum` is `PN_XNUM`: more program headers than the header can count.
    TooManyProgramHeaders,
    /// `PT_INTERP` (ONBF §6.1.2).
    Interp,
    /// `PT_TLS`: thread-local storage is not part of the KRN contract.
    Tls,
    /// `PT_GNU_RELRO`: link with `-z norelro`; KRN segments are RW or read-only.
    GnuRelro,
    /// A program header type outside the contract (for example `PT_NULL` or
    /// `PT_GNU_PROPERTY`).
    UnsupportedProgramHeader(u32),
    /// A second `PT_DYNAMIC` or `PT_GNU_STACK`.
    DuplicateProgramHeader(u32),
    /// `PT_GNU_STACK` is executable (ONBF §6.1.2).
    ExecutableStack,
    /// A `PT_LOAD` that is writable and executable (ONBF-R5).
    WriteAndExecute {
        /// Program header index.
        header: u16,
    },
    /// A `PT_LOAD` whose flags are not exactly R, R+X or R+W.
    SegmentFlags {
        /// Program header index.
        header: u16,
        /// `p_flags`.
        flags: u32,
    },
    /// A `PT_LOAD` whose `p_vaddr` or `p_offset` is not a multiple of 4096, or whose
    /// `p_align` is not 4096.
    SegmentAlignment {
        /// Program header index.
        header: u16,
    },
    /// A `PT_LOAD` with `p_filesz > p_memsz`.
    FileSizeExceedsMemSize {
        /// Program header index.
        header: u16,
    },
    /// A `PT_LOAD` with `p_memsz = 0` (errata C14: a segment must map memory).
    EmptySegment {
        /// Program header index.
        header: u16,
    },
    /// A `PT_LOAD` that starts below the page-rounded end of the previous one: overlap,
    /// a shared page, or descending order (ONBF §6.1, errata G4).
    SegmentOrder {
        /// Program header index.
        header: u16,
    },
    /// The page-rounded end of a segment is beyond 2 GiB (ONBF §6.1.2).
    ImageSpan,
    /// The input has no `PT_LOAD`.
    NoLoadSegments,
    /// `e_entry` is not inside the file-backed bytes of an RX segment.
    EntryNotExecutable(u64),
    /// The dynamic table is malformed.
    DynamicTable(&'static str),
    /// A dynamic tag outside the contract (ONBF §6.1.2: "an unknown dynamic tag").
    UnknownDynamicTag(u64),
    /// `DT_NEEDED`: a KRN image links no libraries.
    Needed,
    /// `DT_INIT`, `DT_FINI` or an init/fini/preinit array: ABI v1 has no static
    /// constructors (ONBF §6.1.2, Appendix B 9).
    StaticConstructors(u64),
    /// A dynamic tag that may appear once appears twice.
    DuplicateDynamicTag(u64),
    /// `DT_RELAENT` is not 24.
    RelaEntrySize(u64),
    /// `DT_RELRENT` is not 8.
    RelrEntrySize(u64),
    /// An incomplete set of relocation tags.
    RelocationTags(&'static str),
    /// A relocation table that is not a whole number of entries, or not inside the
    /// file-backed bytes of one `PT_LOAD`.
    RelocationTable(&'static str),
    /// `DT_RELACOUNT` exceeds the number of `DT_RELA` entries.
    RelaCount {
        /// `DT_RELACOUNT`.
        count: u64,
        /// Entries in `DT_RELA`.
        entries: u64,
    },
    /// A relocation type other than `R_X86_64_RELATIVE`.
    UnsupportedRelocation {
        /// `r_offset`.
        slot: u64,
        /// `ELF64_R_TYPE(r_info)`.
        r_type: u32,
    },
    /// A `R_X86_64_RELATIVE` relocation that names a symbol.
    RelocationSymbol {
        /// `r_offset`.
        slot: u64,
        /// `ELF64_R_SYM(r_info)`.
        symbol: u32,
    },
    /// A relocation slot outside the contract.
    RelocationSlot {
        /// The slot's RVA.
        slot: u64,
        /// What is wrong with it.
        problem: SlotProblem,
    },
    /// Two relocations at one slot that do not agree: two `DT_RELA` entries with
    /// different addends, or a slot in both `DT_RELA` and `DT_RELR`.
    RelocationConflict {
        /// The slot's RVA.
        slot: u64,
    },
    /// The `DT_RELR` table does not decode.
    Relr(RelrError),
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ElfError as E;
        match *self {
            E::Truncated(what) => write!(f, "the input ends inside the {what}"),
            E::NotElf => f.write_str("not an ELF file"),
            E::Elf32 { machine } => write!(
                f,
                "ELFCLASS32 object (e_machine {machine}); ONBF is 64-bit only (ONBF-R11)"
            ),
            E::X32 => f.write_str("x32 (ILP32) object; ONBF is LP64 only (ONBF-R11)"),
            E::I386 => f.write_str("EM_386 (IA-32) object; ONBF is x86-64 only (ONBF-R11)"),
            E::UnsupportedClass(c) => write!(f, "unsupported EI_CLASS {c}"),
            E::UnsupportedDataEncoding(d) => {
                write!(f, "EI_DATA {d} is not ELFDATA2LSB (little-endian)")
            }
            E::UnsupportedVersion(v) => write!(f, "ELF version {v} is not EV_CURRENT"),
            E::NotPie(t) => write!(f, "e_type {t} is not ET_DYN; link a static PIE"),
            E::UnsupportedMachine(m) => write!(f, "e_machine {m} is not EM_X86_64"),
            E::HeaderSize { ehsize, phentsize } => write!(
                f,
                "e_ehsize {ehsize} / e_phentsize {phentsize}; ELF64 needs 64 / 56"
            ),
            E::TooManyProgramHeaders => f.write_str("e_phnum is PN_XNUM; too many program headers"),
            E::Interp => f.write_str("PT_INTERP is not allowed (ONBF §6.1.2)"),
            E::Tls => f.write_str("PT_TLS is not allowed in a KRN image"),
            E::GnuRelro => f.write_str("PT_GNU_RELRO is not allowed; link with -z norelro"),
            E::UnsupportedProgramHeader(t) => {
                write!(f, "program header type {t:#x} is outside the contract")
            }
            E::DuplicateProgramHeader(t) => write!(f, "program header type {t:#x} appears twice"),
            E::ExecutableStack => f.write_str("PT_GNU_STACK is executable (ONBF §6.1.2)"),
            E::WriteAndExecute { header } => {
                write!(f, "PT_LOAD {header} is writable and executable (ONBF-R5)")
            }
            E::SegmentFlags { header, flags } => write!(
                f,
                "PT_LOAD {header} has flags {flags:#x}; only R, R+X and R+W are allowed"
            ),
            E::SegmentAlignment { header } => write!(
                f,
                "PT_LOAD {header}: p_vaddr and p_offset must be multiples of 4096 and p_align 4096 (link with -z separate-loadable-segments -z max-page-size=4096)"
            ),
            E::FileSizeExceedsMemSize { header } => {
                write!(f, "PT_LOAD {header} has p_filesz > p_memsz")
            }
            E::EmptySegment { header } => write!(f, "PT_LOAD {header} has p_memsz 0"),
            E::SegmentOrder { header } => write!(
                f,
                "PT_LOAD {header} overlaps or precedes the previous segment's last page"
            ),
            E::ImageSpan => f.write_str("the image span exceeds 2 GiB"),
            E::NoLoadSegments => f.write_str("the input has no PT_LOAD"),
            E::EntryNotExecutable(e) => write!(
                f,
                "e_entry {e:#x} is not inside the file-backed bytes of an R+X segment"
            ),
            E::DynamicTable(why) => write!(f, "malformed dynamic table: {why}"),
            E::UnknownDynamicTag(t) => write!(
                f,
                "dynamic tag {t:#x} is outside the contract (ONBF §6.1.2)"
            ),
            E::Needed => f.write_str("DT_NEEDED: a KRN image links no libraries"),
            E::StaticConstructors(t) => write!(
                f,
                "dynamic tag {t:#x}: static constructors and destructors are not allowed (ONBF §6.1.2)"
            ),
            E::DuplicateDynamicTag(t) => write!(f, "dynamic tag {t:#x} appears twice"),
            E::RelaEntrySize(n) => write!(f, "DT_RELAENT is {n}, not 24"),
            E::RelrEntrySize(n) => write!(f, "DT_RELRENT is {n}, not 8"),
            E::RelocationTags(why) => write!(f, "relocation tags: {why}"),
            E::RelocationTable(why) => write!(f, "relocation table: {why}"),
            E::RelaCount { count, entries } => write!(
                f,
                "DT_RELACOUNT {count} exceeds the {entries} DT_RELA entries"
            ),
            E::UnsupportedRelocation { slot, r_type } => write!(
                f,
                "relocation type {r_type} at {slot:#x}; only R_X86_64_RELATIVE is allowed"
            ),
            E::RelocationSymbol { slot, symbol } => {
                write!(f, "R_X86_64_RELATIVE at {slot:#x} names symbol {symbol}")
            }
            E::RelocationSlot { slot, problem } => {
                let why = match problem {
                    SlotProblem::Misaligned => "is not 8-byte aligned",
                    SlotProblem::Unmapped => "is outside every PT_LOAD",
                    SlotProblem::NotWritable(Permissions::ReadExecute) => {
                        "is in an R+X segment (a text relocation)"
                    }
                    SlotProblem::NotWritable(_) => "is in a read-only segment",
                    SlotProblem::Bss => "is in the zero-filled (bss) part of its segment",
                };
                write!(f, "relocation slot {slot:#x} {why}")
            }
            E::RelocationConflict { slot } => write!(
                f,
                "conflicting relocations at {slot:#x} (different addends, or both DT_RELA and DT_RELR)"
            ),
            E::Relr(e) => write!(f, "DT_RELR: {e}"),
        }
    }
}

impl std::error::Error for ElfError {}

/// Rounds `x` up to a whole number of pages. `None` on overflow.
pub(crate) fn page_align(x: u64) -> Option<u64> {
    Some(x.checked_add(PAGE_MASK)? & !PAGE_MASK)
}

/// `bytes[offset..offset + len]`, or `None` if any part is outside the input.
fn range(bytes: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(len).ok()?;
    bytes.get(start..start.checked_add(len)?)
}

/// Little-endian fields of an already bounds-checked structure. A failed read reports
/// the structure as truncated, which cannot happen for the fixed-size slices used here.
struct Fields<'a> {
    bytes: &'a [u8],
    what: &'static str,
}

impl Fields<'_> {
    fn array<const N: usize>(&self, at: usize) -> Result<[u8; N], ElfError> {
        at.checked_add(N)
            .and_then(|end| self.bytes.get(at..end))
            .and_then(|b| <[u8; N]>::try_from(b).ok())
            .ok_or(ElfError::Truncated(self.what))
    }
    fn u16(&self, at: usize) -> Result<u16, ElfError> {
        self.array(at).map(u16::from_le_bytes)
    }
    fn u32(&self, at: usize) -> Result<u32, ElfError> {
        self.array(at).map(u32::from_le_bytes)
    }
    fn u64(&self, at: usize) -> Result<u64, ElfError> {
        self.array(at).map(u64::from_le_bytes)
    }
}

/// Validates `file` as an ELF64 x86-64 PIE under the KRN conversion contract.
pub fn parse(file: &[u8]) -> Result<Elf, ElfError> {
    let FileHeader {
        entry,
        phoff,
        phnum,
    } = check_header(file)?;
    let table_len = u64::from(phnum)
        .checked_mul(u64::from(PHDR_SIZE))
        .ok_or(ElfError::Truncated("program header table"))?;
    let table = range(file, phoff, table_len).ok_or(ElfError::Truncated("program header table"))?;

    let mut segments: Vec<LoadSegment> = Vec::new();
    let mut dynamic: Option<(u64, u64)> = None;
    let mut stack_seen = false;
    for (i, raw) in table.as_chunks::<PHDR_BYTES>().0.iter().enumerate() {
        let index = u16::try_from(i).map_err(|_| ElfError::TooManyProgramHeaders)?;
        let ph = ProgramHeader::decode(raw)?;
        match ph.p_type {
            pt::LOAD => {
                let segment = check_load(file, index, &ph)?;
                if let Some(prev) = segments.last() {
                    let prev_end = prev.vaddr.saturating_add(prev.mapped_size);
                    if segment.vaddr < prev_end {
                        return Err(ElfError::SegmentOrder { header: index });
                    }
                }
                segments.push(segment);
            }
            pt::DYNAMIC => {
                if dynamic.replace((ph.offset, ph.file_size)).is_some() {
                    return Err(ElfError::DuplicateProgramHeader(ph.p_type));
                }
            }
            pt::GNU_STACK => {
                if stack_seen {
                    return Err(ElfError::DuplicateProgramHeader(ph.p_type));
                }
                stack_seen = true;
                if ph.flags & PF_X != 0 {
                    return Err(ElfError::ExecutableStack);
                }
            }
            // PT_GNU_EH_FRAME only locates `.eh_frame_hdr`, which lies inside the R
            // segment and is carried with it (coordinator amendment, 2026-11-07). Unwind
            // data becomes an UNWIND section in a later version.
            pt::NOTE | pt::PHDR | pt::GNU_EH_FRAME => {}
            pt::INTERP => return Err(ElfError::Interp),
            pt::TLS => return Err(ElfError::Tls),
            pt::GNU_RELRO => return Err(ElfError::GnuRelro),
            other => return Err(ElfError::UnsupportedProgramHeader(other)),
        }
    }
    if segments.is_empty() {
        return Err(ElfError::NoLoadSegments);
    }
    let in_text = segments
        .iter()
        .any(|s| s.permissions == Permissions::ReadExecute && s.file_backed(entry, 1));
    if !in_text {
        return Err(ElfError::EntryNotExecutable(entry));
    }

    let relocations = match dynamic {
        Some((offset, size)) => read_dynamic(file, offset, size, &segments)?,
        None => Vec::new(),
    };
    Ok(Elf {
        entry,
        segments,
        relocations,
    })
}

/// The ELF header fields the rest of the parser needs.
struct FileHeader {
    entry: u64,
    phoff: u64,
    phnum: u16,
}

/// Checks `e_ident` and the ELF header.
fn check_header(file: &[u8]) -> Result<FileHeader, ElfError> {
    match file.get(..ELF_MAGIC.len()) {
        None => return Err(ElfError::Truncated("ELF identification")),
        Some(m) if m != ELF_MAGIC => return Err(ElfError::NotElf),
        Some(_) => {}
    }
    let ident = file
        .get(..EI_NIDENT)
        .ok_or(ElfError::Truncated("ELF identification"))?;
    let byte = |i: usize| ident.get(i).copied().unwrap_or(0);
    let (class, data, version) = (byte(4), byte(5), byte(6));
    if class == ELFCLASS32 {
        // e_machine is at offset 18 in both classes, so a 32-bit object can be named
        // precisely without decoding the rest of its header (ONBF-R11).
        let raw = file
            .get(18..20)
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .ok_or(ElfError::Truncated("ELF header"))?;
        let machine = if data == ELFDATA2MSB {
            u16::from_be_bytes(raw)
        } else {
            u16::from_le_bytes(raw)
        };
        return Err(match machine {
            EM_X86_64 => ElfError::X32,
            EM_386 => ElfError::I386,
            machine => ElfError::Elf32 { machine },
        });
    }
    if class != ELFCLASS64 {
        return Err(ElfError::UnsupportedClass(class));
    }
    if data != ELFDATA2LSB {
        return Err(ElfError::UnsupportedDataEncoding(data));
    }
    if u32::from(version) != EV_CURRENT {
        return Err(ElfError::UnsupportedVersion(u32::from(version)));
    }
    let h = Fields {
        bytes: file
            .get(..usize::from(EHDR_SIZE))
            .ok_or(ElfError::Truncated("ELF header"))?,
        what: "ELF header",
    };
    let e_type = h.u16(16)?;
    let e_machine = h.u16(18)?;
    let e_version = h.u32(20)?;
    let e_entry = h.u64(24)?;
    let e_phoff = h.u64(32)?;
    let e_ehsize = h.u16(52)?;
    let e_phentsize = h.u16(54)?;
    let e_phnum = h.u16(56)?;
    if e_machine == EM_386 {
        return Err(ElfError::I386);
    }
    if e_machine != EM_X86_64 {
        return Err(ElfError::UnsupportedMachine(e_machine));
    }
    if e_type != ET_DYN {
        return Err(ElfError::NotPie(e_type));
    }
    if e_version != EV_CURRENT {
        return Err(ElfError::UnsupportedVersion(e_version));
    }
    if e_ehsize != EHDR_SIZE || e_phentsize != PHDR_SIZE {
        return Err(ElfError::HeaderSize {
            ehsize: e_ehsize,
            phentsize: e_phentsize,
        });
    }
    if e_phnum == PN_XNUM {
        return Err(ElfError::TooManyProgramHeaders);
    }
    Ok(FileHeader {
        entry: e_entry,
        phoff: e_phoff,
        phnum: e_phnum,
    })
}

/// One decoded `Elf64_Phdr` (`p_paddr` is not used).
struct ProgramHeader {
    p_type: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    file_size: u64,
    mem_size: u64,
    align: u64,
}

impl ProgramHeader {
    fn decode(raw: &[u8]) -> Result<Self, ElfError> {
        let ph = Fields {
            bytes: raw,
            what: "program header table",
        };
        Ok(Self {
            p_type: ph.u32(0)?,
            flags: ph.u32(4)?,
            offset: ph.u64(8)?,
            vaddr: ph.u64(16)?,
            file_size: ph.u64(32)?,
            mem_size: ph.u64(40)?,
            align: ph.u64(48)?,
        })
    }
}

/// Validates one `PT_LOAD` on its own (ordering is checked by the caller).
fn check_load(file: &[u8], header: u16, ph: &ProgramHeader) -> Result<LoadSegment, ElfError> {
    let &ProgramHeader {
        flags,
        offset,
        vaddr,
        file_size,
        mem_size,
        align,
        ..
    } = ph;
    if flags & (PF_W | PF_X) == PF_W | PF_X {
        return Err(ElfError::WriteAndExecute { header });
    }
    let permissions = match flags {
        f if f == PF_R => Permissions::Read,
        f if f == PF_R | PF_X => Permissions::ReadExecute,
        f if f == PF_R | PF_W => Permissions::ReadWrite,
        flags => return Err(ElfError::SegmentFlags { header, flags }),
    };
    if align != PAGE_SIZE || vaddr & PAGE_MASK != 0 || offset & PAGE_MASK != 0 {
        return Err(ElfError::SegmentAlignment { header });
    }
    if file_size > mem_size {
        return Err(ElfError::FileSizeExceedsMemSize { header });
    }
    if mem_size == 0 {
        return Err(ElfError::EmptySegment { header });
    }
    let mapped_size = page_align(mem_size).ok_or(ElfError::ImageSpan)?;
    vaddr
        .checked_add(mapped_size)
        .filter(|&end| end <= MAX_IMAGE_SPAN)
        .ok_or(ElfError::ImageSpan)?;
    range(file, offset, file_size).ok_or(ElfError::Truncated("file range of a PT_LOAD"))?;
    Ok(LoadSegment {
        header,
        permissions,
        vaddr,
        file_offset: offset,
        file_size,
        mapped_size,
    })
}

/// The relocation-related dynamic tags, each at most once.
#[derive(Default)]
struct RelocationTags {
    rela: Option<u64>,
    relasz: Option<u64>,
    relaent: Option<u64>,
    relacount: Option<u64>,
    relr: Option<u64>,
    relrsz: Option<u64>,
    relrent: Option<u64>,
}

impl RelocationTags {
    fn set(&mut self, tag: u64, value: u64) -> Result<(), ElfError> {
        let field = match tag {
            dt::RELA => &mut self.rela,
            dt::RELASZ => &mut self.relasz,
            dt::RELAENT => &mut self.relaent,
            dt::RELACOUNT => &mut self.relacount,
            dt::RELR => &mut self.relr,
            dt::RELRSZ => &mut self.relrsz,
            _ => &mut self.relrent,
        };
        if field.replace(value).is_some() {
            return Err(ElfError::DuplicateDynamicTag(tag));
        }
        Ok(())
    }
}

/// Reads the dynamic table and returns the validated, merged relocation slots.
fn read_dynamic(
    file: &[u8],
    offset: u64,
    size: u64,
    segments: &[LoadSegment],
) -> Result<Vec<Relocation>, ElfError> {
    let table = range(file, offset, size).ok_or(ElfError::Truncated("dynamic table"))?;
    if table.len() % DYN_SIZE != 0 {
        return Err(ElfError::DynamicTable(
            "PT_DYNAMIC size is not a multiple of 16",
        ));
    }
    let mut tags = RelocationTags::default();
    let mut terminated = false;
    for raw in table.as_chunks::<DYN_SIZE>().0 {
        let d = Fields {
            bytes: raw,
            what: "dynamic table",
        };
        let tag = d.u64(0)?;
        let value = d.u64(8)?;
        match tag {
            dt::NULL => {
                terminated = true;
                break;
            }
            dt::NEEDED => return Err(ElfError::Needed),
            dt::INIT
            | dt::FINI
            | dt::INIT_ARRAY
            | dt::FINI_ARRAY
            | dt::INIT_ARRAYSZ
            | dt::FINI_ARRAYSZ
            | dt::PREINIT_ARRAY
            | dt::PREINIT_ARRAYSZ => return Err(ElfError::StaticConstructors(tag)),
            dt::RELA
            | dt::RELASZ
            | dt::RELAENT
            | dt::RELACOUNT
            | dt::RELR
            | dt::RELRSZ
            | dt::RELRENT => tags.set(tag, value)?,
            // Symbol and hash tables are only metadata here: no relocation may name a
            // symbol, so nothing reads them. DT_FLAGS, DT_FLAGS_1 and DT_DEBUG carry no
            // information a KRN image needs.
            dt::HASH
            | dt::GNU_HASH
            | dt::SYMTAB
            | dt::STRTAB
            | dt::STRSZ
            | dt::SYMENT
            | dt::FLAGS
            | dt::FLAGS_1
            | dt::DEBUG => {}
            other => return Err(ElfError::UnknownDynamicTag(other)),
        }
    }
    if !terminated {
        return Err(ElfError::DynamicTable("no DT_NULL terminator"));
    }

    let rela = read_rela(file, &tags, segments)?;
    let relr = read_relr(file, &tags, segments)?;
    merge(rela, relr)
}

/// The bytes of a relocation table at RVA `rva`, which must lie inside the file-backed
/// bytes of one segment.
fn table_bytes<'a>(
    file: &'a [u8],
    segments: &[LoadSegment],
    rva: u64,
    size: u64,
    what: &'static str,
) -> Result<&'a [u8], ElfError> {
    if size == 0 {
        return Ok(&[]);
    }
    let segment = segments
        .iter()
        .find(|s| s.file_backed(rva, size))
        .ok_or(ElfError::RelocationTable(what))?;
    let start = rva
        .checked_sub(segment.vaddr)
        .and_then(|d| segment.file_offset.checked_add(d))
        .ok_or(ElfError::RelocationTable(what))?;
    range(file, start, size).ok_or(ElfError::RelocationTable(what))
}

/// Checks one relocation slot against the segments (see [`SlotProblem`]).
fn check_slot(slot: u64, segments: &[LoadSegment]) -> Result<(), ElfError> {
    let problem = |problem| ElfError::RelocationSlot { slot, problem };
    if slot & 7 != 0 {
        return Err(problem(SlotProblem::Misaligned));
    }
    let segment = segments
        .iter()
        .find(|s| s.maps(slot))
        .ok_or(problem(SlotProblem::Unmapped))?;
    if segment.permissions != Permissions::ReadWrite {
        return Err(problem(SlotProblem::NotWritable(segment.permissions)));
    }
    if !segment.file_backed(slot, 8) {
        return Err(problem(SlotProblem::Bss));
    }
    Ok(())
}

/// The `DT_RELA` entries as `(slot, addend)`, sorted by slot, with exact duplicates removed.
fn read_rela(
    file: &[u8],
    tags: &RelocationTags,
    segments: &[LoadSegment],
) -> Result<Vec<(u64, u64)>, ElfError> {
    let (rva, size) = match (tags.rela, tags.relasz, tags.relaent) {
        (None, None, None) => {
            if tags.relacount.is_some() {
                return Err(ElfError::RelocationTags("DT_RELACOUNT without DT_RELA"));
            }
            return Ok(Vec::new());
        }
        (Some(rva), Some(size), Some(entry)) => {
            if entry != RELA_SIZE {
                return Err(ElfError::RelaEntrySize(entry));
            }
            (rva, size)
        }
        _ => {
            return Err(ElfError::RelocationTags(
                "DT_RELA, DT_RELASZ and DT_RELAENT must appear together",
            ));
        }
    };
    if size % RELA_SIZE != 0 {
        return Err(ElfError::RelocationTable(
            "DT_RELASZ is not a multiple of 24",
        ));
    }
    let bytes = table_bytes(
        file,
        segments,
        rva,
        size,
        "DT_RELA is not inside the file-backed bytes of one PT_LOAD",
    )?;
    let entries = size / RELA_SIZE;
    if let Some(count) = tags.relacount
        && count > entries
    {
        return Err(ElfError::RelaCount { count, entries });
    }
    let mut out = Vec::new();
    for raw in bytes.as_chunks::<RELA_BYTES>().0 {
        let r = Fields {
            bytes: raw,
            what: "DT_RELA table",
        };
        let slot = r.u64(0)?;
        let info = r.u64(8)?;
        let addend = r.u64(16)?;
        let r_type = u32::try_from(info & 0xFFFF_FFFF).unwrap_or(u32::MAX);
        let symbol = u32::try_from(info >> 32).unwrap_or(u32::MAX);
        if r_type != R_X86_64_RELATIVE {
            return Err(ElfError::UnsupportedRelocation { slot, r_type });
        }
        if symbol != 0 {
            return Err(ElfError::RelocationSymbol { slot, symbol });
        }
        check_slot(slot, segments)?;
        out.push((slot, addend));
    }
    // Two RELA entries at one slot write the same value only if their addends agree; then
    // they are one relocation. Otherwise the result would depend on the order applied.
    out.sort_unstable();
    out.dedup();
    for pair in out.windows(2) {
        if let [(a, _), (b, _)] = pair
            && a == b
        {
            return Err(ElfError::RelocationConflict { slot: *a });
        }
    }
    Ok(out)
}

/// The `DT_RELR` slots, strictly ascending.
fn read_relr(
    file: &[u8],
    tags: &RelocationTags,
    segments: &[LoadSegment],
) -> Result<Vec<u64>, ElfError> {
    let (rva, size) = match (tags.relr, tags.relrsz, tags.relrent) {
        (None, None, None) => return Ok(Vec::new()),
        (Some(rva), Some(size), Some(entry)) => {
            if entry != RELR_SIZE {
                return Err(ElfError::RelrEntrySize(entry));
            }
            (rva, size)
        }
        _ => {
            return Err(ElfError::RelocationTags(
                "DT_RELR, DT_RELRSZ and DT_RELRENT must appear together",
            ));
        }
    };
    if size % RELR_SIZE != 0 {
        return Err(ElfError::RelocationTable(
            "DT_RELRSZ is not a multiple of 8",
        ));
    }
    let bytes = table_bytes(
        file,
        segments,
        rva,
        size,
        "DT_RELR is not inside the file-backed bytes of one PT_LOAD",
    )?;
    let words = relr::words_from_bytes(bytes).map_err(ElfError::Relr)?;
    let slots = relr::decode(&words).map_err(ElfError::Relr)?;
    for &slot in &slots {
        check_slot(slot, segments)?;
    }
    Ok(slots)
}

/// Merges the two sorted slot lists. A slot in both is a conflict: `DT_RELA` would write
/// `B + A` and `DT_RELR` would then add `B` again, depending on the order a loader picks.
fn merge(rela: Vec<(u64, u64)>, relr: Vec<u64>) -> Result<Vec<Relocation>, ElfError> {
    let mut out = Vec::with_capacity(rela.len().saturating_add(relr.len()));
    let mut a = rela.into_iter().peekable();
    let mut b = relr.into_iter().peekable();
    loop {
        let next = match (a.peek(), b.peek()) {
            (None, None) => break,
            (Some(&(slot, _)), Some(&other)) if slot == other => {
                return Err(ElfError::RelocationConflict { slot });
            }
            (Some(&(slot, addend)), Some(&other)) if slot < other => {
                a.next();
                Relocation {
                    slot,
                    addend: Some(addend),
                }
            }
            (Some(&(slot, addend)), None) => {
                a.next();
                Relocation {
                    slot,
                    addend: Some(addend),
                }
            }
            (_, Some(&slot)) => {
                b.next();
                Relocation { slot, addend: None }
            }
        };
        out.push(next);
    }
    Ok(out)
}
