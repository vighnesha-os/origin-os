//! ELF64 input, read against the conversion contract (ONBF §6.1.2, ONBF-R11): for the KRN
//! image (`krn`), and for the programs the kernel carries in it (`carried`, ADR-0011 L1).
//!
//! Sources: the System V ABI generic part (ELF gABI: `e_ident`, the ELF header, program
//! headers, the dynamic section, `Elf64_Rela`, and the `SHT_RELR` encoding) and the x86-64
//! psABI (`EM_X86_64`, `R_X86_64_RELATIVE`). Every field is decoded explicitly from bytes,
//! with no struct casts (ONBF-R7).
//!
//! The parser accepts exactly the shape that `rust-lld` writes for the Framekernel, a
//! static PIE linked with `--no-dynamic-linker -z text -z separate-loadable-segments
//! -z max-page-size=4096 -z common-page-size=4096 -z relro -z now -z noexecstack`, and rejects
//! everything else with a typed [`ElfError`]. It returns validated data only: no partial
//! results.
//!
//! Program headers: `PT_LOAD`; `PT_DYNAMIC`, `PT_GNU_STACK` (non-executable) and
//! `PT_GNU_RELRO`, at most once each; `PT_NOTE`, `PT_PHDR` and `PT_GNU_EH_FRAME`, which are
//! ignored. Anything else is a hard error. `PT_GNU_RELRO` must be whole pages at the start of
//! an R+W `PT_LOAD`: `rust-lld` also ends that `PT_LOAD` there, and if a linker does not, the
//! rest is split off, so the two never share a page (ONBF §6.1).
//!
//! The dynamic symbol table is read only to refuse an undefined symbol (§6.1.2), its length
//! taken from `DT_HASH`; no relocation may name a symbol, so nothing else in it is used.
//!
//! That is [`parse`], the reading of a static PIE. Two more readings exist since task S4-2
//! (ADR-0011 Amendment 2, L2 and L3), each chosen by its caller with [`Linking`] and each as
//! strict about everything else:
//!
//! - [`Linking::Library`], for `origin.core`: a shared object with a `DT_SONAME`, no entry point,
//!   no dependency and nothing undefined. Its defined dynamic symbols are returned as exports.
//! - [`Linking::Importing`], for a program linked against one named library: its one
//!   `DT_NEEDED` names that library, each undefined symbol is one of the library's exports, and
//!   an `R_X86_64_GLOB_DAT` relocation against one becomes an import slot, which the kernel's
//!   launch path binds. `R_X86_64_JUMP_SLOT` is not read: the userspace build calls an import
//!   through its own slot and writes no procedure linkage table, and `DT_JMPREL` and its
//!   companions stay outside the contract, so an image that had one is refused by its tags.
//!
//! Further sources for those two, read on 2026-11-13 through a fetch tool that paraphrases: the
//! gABI's dynamic section (`DT_NEEDED` 1 and `DT_SONAME` 14, each an offset into `DT_STRTAB`)
//! and its symbol table (`Elf64_Sym`'s fields, `st_info`'s binding in the upper four bits and
//! type in the lower, `st_other`'s visibility in the low two), at
//! `sco.com/developers/gabi/latest`, which carries no date but its 1997 to 2015 copyright
//! lines; and the x86-64 psABI's relocation table at commit `e1ce0983` (`R_X86_64_GLOB_DAT` 6
//! and `R_X86_64_JUMP_SLOT` 7, both `S`; `R_X86_64_RELATIVE` 8, `B + A`).

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
    /// The shared object's own name.
    pub const SONAME: u64 = 14;
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
/// `Elf64_Sym`'s size, the only accepted `DT_SYMENT`.
const SYM_SIZE: u64 = 24;
/// The same, for slicing.
const SYM_BYTES: usize = 24;
/// Where `st_shndx` is in an `Elf64_Sym`.
const SYM_SHNDX_AT: usize = 6;
/// `st_shndx` of a symbol the image does not define (gABI `SHN_UNDEF`).
const SHN_UNDEF: u16 = 0;
/// Where `st_name`, `st_info`, `st_other`, `st_value` and `st_size` are in an `Elf64_Sym`.
const SYM_NAME_AT: usize = 0;
const SYM_INFO_AT: usize = 4;
const SYM_OTHER_AT: usize = 5;
const SYM_VALUE_AT: usize = 8;
const SYM_SIZE_AT: usize = 16;
/// `ELF64_ST_BIND` of a global symbol, and `ELF64_ST_TYPE` of a function (gABI).
const STB_GLOBAL: u8 = 1;
const STT_FUNC: u8 = 2;
const RELA_SIZE: u64 = 24;
const RELA_BYTES: usize = 24;
const RELR_SIZE: u64 = 8;
/// `B + A`: the only relocation type a KRN image may carry (x86-64 psABI).
pub const R_X86_64_RELATIVE: u32 = 8;
/// `S`: the address of the symbol it names, for a slot of the global offset table (x86-64
/// psABI). Read only for a program that imports from a named library ([`Linking::Importing`]).
pub const R_X86_64_GLOB_DAT: u32 = 6;

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
    /// Read-only once relocated: the part of an R+W `PT_LOAD` that `PT_GNU_RELRO` covers.
    pub relro: bool,
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

/// How an input's dynamic part is read: what it may need, and what it may leave undefined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Linking<'a> {
    /// A static PIE: no library, no undefined symbol, and base-relative relocations only. The
    /// KRN image, and a program that imports nothing.
    Static,
    /// A program linked against one Origin library (ONBF §6.1.2: an undefined symbol must be
    /// "exported by a declared Origin library"). `DT_NEEDED` may name `library` and nothing
    /// else; an undefined symbol must be one of `exports`; and an `R_X86_64_GLOB_DAT`
    /// relocation against one is an import.
    Importing {
        /// The library's `DT_SONAME`.
        library: &'a str,
        /// The symbols it exports.
        exports: &'a [&'a str],
    },
    /// A library with no dependency of its own: it has a `DT_SONAME` and no entry point, needs
    /// nothing and leaves nothing undefined.
    Library,
}

/// A slot the launch path binds: it takes the address of the export it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// RVA of the 8-byte slot; 8-byte aligned, in the file-backed bytes of an RW segment.
    pub slot: u64,
    /// The symbol's name: one of the library's exports.
    pub symbol: String,
}

/// A symbol a library's dynamic symbol table defines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// Its name.
    pub name: String,
    /// `st_value`: its RVA.
    pub value: u64,
    /// `st_size`: its bytes.
    pub size: u64,
    /// Whether it is a function (`STT_FUNC`).
    pub function: bool,
    /// Whether it is a global symbol of default visibility: what another image may bind to.
    pub public: bool,
}

/// A validated input image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elf {
    /// `e_entry`: inside the file-backed bytes of an RX segment, or 0 for a library.
    pub entry: u64,
    /// Loadable segments, in ascending, non-overlapping page-rounded RVA order.
    pub segments: Vec<LoadSegment>,
    /// Relocation slots in strictly ascending order, from `DT_RELA` and `DT_RELR` merged.
    pub relocations: Vec<Relocation>,
    /// Import slots in strictly ascending order, none of them a relocation slot. Empty unless
    /// the input was read as [`Linking::Importing`].
    pub imports: Vec<Import>,
    /// The symbols the image defines, in the order of its dynamic symbol table. Empty unless
    /// the input was read as [`Linking::Library`].
    pub exports: Vec<Export>,
    /// `DT_SONAME`. `None` unless the input was read as [`Linking::Library`].
    pub soname: Option<String>,
    /// `PT_DYNAMIC`'s place in the file: its offset and its size. The table is the linker's
    /// own record, read here and by nothing at run time.
    pub dynamic_table: Option<(u64, u64)>,
    /// Whether the table's first `DT_NULL` is its last entry, so that every entry of
    /// `PT_DYNAMIC` was read. `true` for an image with no table. The reader stops at the first
    /// `DT_NULL`, as a loader would; a caller that must know nothing follows it asks here.
    pub dynamic_null_last: bool,
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
    /// `PT_GNU_RELRO` is not a whole number of pages at the start of one R+W `PT_LOAD`.
    RelroRange {
        /// `p_vaddr`.
        start: u64,
        /// `p_memsz`.
        size: u64,
    },
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
    /// `DT_NEEDED`, in an image that may link no library.
    Needed,
    /// A program leaves a symbol to the library it imports from and has no `DT_NEEDED` at all.
    NeededMissing,
    /// A `DT_NEEDED` that does not name the one library the program may import from, or a
    /// second one.
    NeededOther,
    /// `DT_SONAME` is missing from a library, appears twice, or does not name a string.
    Soname(&'static str),
    /// A library with an entry point: `e_entry` is not 0.
    LibraryEntry(u64),
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
    /// A relocation type other than `R_X86_64_RELATIVE`, or, for a program that imports from
    /// a named library, `R_X86_64_GLOB_DAT`.
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
    /// The dynamic symbol table cannot be read: `DT_SYMTAB` without `DT_HASH`, which gives its
    /// length, a `DT_SYMENT` other than 24, or a table outside the file-backed bytes of one
    /// `PT_LOAD`.
    SymbolTable(&'static str),
    /// A dynamic symbol the image does not define (ONBF §6.1.2: "an undefined symbol not
    /// exported by a declared Origin library"): no library is declared, or the declared one
    /// does not export it.
    UndefinedSymbol {
        /// Its index in the dynamic symbol table.
        index: u64,
    },
    /// A symbol whose name is outside `DT_STRTAB`, has no terminator there, or is not UTF-8.
    SymbolName {
        /// Its index in the dynamic symbol table.
        index: u64,
    },
    /// An `R_X86_64_GLOB_DAT` relocation that names no symbol, one past the table's end, or
    /// one the image defines itself.
    ImportSymbol {
        /// `r_offset`.
        slot: u64,
        /// `ELF64_R_SYM(r_info)`.
        symbol: u32,
    },
    /// An `R_X86_64_GLOB_DAT` relocation with an addend: its value is the symbol's address
    /// alone.
    ImportAddend {
        /// `r_offset`.
        slot: u64,
    },
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
            E::RelroRange { start, size } => write!(
                f,
                "PT_GNU_RELRO {start:#x}+{size:#x} is not whole pages at the start of one R+W PT_LOAD"
            ),
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
            E::Needed => f.write_str("DT_NEEDED: this image may link no library"),
            E::NeededMissing => f.write_str(
                "an undefined symbol of the library a program imports from, and no DT_NEEDED that names that library",
            ),
            E::NeededOther => f.write_str(
                "DT_NEEDED names something other than the one library this program may import from, or appears twice",
            ),
            E::Soname(why) => write!(f, "DT_SONAME: {why}"),
            E::LibraryEntry(e) => write!(f, "a library has no entry point, but e_entry is {e:#x}"),
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
                "relocation type {r_type} at {slot:#x}; only R_X86_64_RELATIVE is allowed, and R_X86_64_GLOB_DAT against an export of the library a program imports from"
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
            E::SymbolTable(why) => write!(f, "dynamic symbol table: {why}"),
            E::UndefinedSymbol { index } => write!(
                f,
                "dynamic symbol {index} is undefined, and no declared Origin library exports it (ONBF §6.1.2)"
            ),
            E::SymbolName { index } => write!(
                f,
                "dynamic symbol {index} has a name outside DT_STRTAB, unterminated, or not UTF-8"
            ),
            E::ImportSymbol { slot, symbol } => write!(
                f,
                "R_X86_64_GLOB_DAT at {slot:#x} names symbol {symbol}, which is not an undefined symbol of the table"
            ),
            E::ImportAddend { slot } => {
                write!(f, "R_X86_64_GLOB_DAT at {slot:#x} has an addend")
            }
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

/// Validates `file` as an ELF64 x86-64 PIE under the KRN conversion contract: a static PIE
/// ([`Linking::Static`]).
pub fn parse(file: &[u8]) -> Result<Elf, ElfError> {
    parse_linked(file, Linking::Static)
}

/// Validates `file` as [`parse`] does, with its dynamic part read as `linking` says.
pub fn parse_linked(file: &[u8], linking: Linking<'_>) -> Result<Elf, ElfError> {
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
    let mut relro: Option<(u64, u64)> = None;
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
            pt::GNU_RELRO => {
                if relro.replace((ph.vaddr, ph.mem_size)).is_some() {
                    return Err(ElfError::DuplicateProgramHeader(ph.p_type));
                }
            }
            other => return Err(ElfError::UnsupportedProgramHeader(other)),
        }
    }
    if segments.is_empty() {
        return Err(ElfError::NoLoadSegments);
    }
    if let Some((start, size)) = relro {
        apply_relro(&mut segments, start, size)?;
    }
    if linking == Linking::Library {
        if entry != 0 {
            return Err(ElfError::LibraryEntry(entry));
        }
    } else {
        let in_text = segments
            .iter()
            .any(|s| s.permissions == Permissions::ReadExecute && s.file_backed(entry, 1));
        if !in_text {
            return Err(ElfError::EntryNotExecutable(entry));
        }
    }

    let dynamic_table = dynamic;
    let dynamic = match dynamic {
        Some((offset, size)) => read_dynamic(file, offset, size, &segments, linking)?,
        None => Dynamic::default(),
    };
    if linking == Linking::Library && dynamic.soname.is_none() {
        return Err(ElfError::Soname("a library must have one"));
    }
    Ok(Elf {
        entry,
        segments,
        relocations: dynamic.relocations,
        imports: dynamic.imports,
        exports: dynamic.exports,
        soname: dynamic.soname,
        dynamic_table,
        dynamic_null_last: dynamic_table.is_none() || dynamic.null_last,
    })
}

/// Marks the first `size` bytes of the R+W `PT_LOAD` starting at `start` read-only after
/// relocation. A remainder becomes its own segment, starting on the page after them.
fn apply_relro(segments: &mut Vec<LoadSegment>, start: u64, size: u64) -> Result<(), ElfError> {
    let bad = ElfError::RelroRange { start, size };
    let index = segments
        .iter()
        .position(|s| s.vaddr == start && s.permissions == Permissions::ReadWrite)
        .ok_or(bad)?;
    let whole = segments.get(index).copied().ok_or(bad)?;
    if size == 0 || size & PAGE_MASK != 0 || size > whole.mapped_size {
        return Err(bad);
    }
    let relro_file = whole.file_size.min(size);
    let rest_file = whole.file_size.saturating_sub(relro_file);
    let rest_mapped = whole.mapped_size.saturating_sub(size);
    if let Some(segment) = segments.get_mut(index) {
        *segment = LoadSegment {
            relro: true,
            file_size: relro_file,
            mapped_size: size,
            ..whole
        };
    }
    if rest_mapped != 0 {
        let rest = LoadSegment {
            vaddr: whole.vaddr.checked_add(size).ok_or(bad)?,
            // With no file bytes left the offset is never read, but it stays a page multiple
            // inside the input.
            file_offset: match rest_file {
                0 => whole.file_offset,
                _ => whole.file_offset.checked_add(size).ok_or(bad)?,
            },
            file_size: rest_file,
            mapped_size: rest_mapped,
            ..whole
        };
        segments.insert(index.saturating_add(1), rest);
    }
    Ok(())
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
        relro: false,
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

/// What the dynamic table gave.
#[derive(Default)]
struct Dynamic {
    relocations: Vec<Relocation>,
    imports: Vec<Import>,
    exports: Vec<Export>,
    soname: Option<String>,
    /// The table's first `DT_NULL` is its last entry.
    null_last: bool,
}

/// Reads the dynamic table as `linking` says, and returns the validated, merged relocation
/// slots with what else that reading gives.
fn read_dynamic(
    file: &[u8],
    offset: u64,
    size: u64,
    segments: &[LoadSegment],
    linking: Linking<'_>,
) -> Result<Dynamic, ElfError> {
    let table = range(file, offset, size).ok_or(ElfError::Truncated("dynamic table"))?;
    if table.len() % DYN_SIZE != 0 {
        return Err(ElfError::DynamicTable(
            "PT_DYNAMIC size is not a multiple of 16",
        ));
    }
    let mut tags = RelocationTags::default();
    let mut symbols = SymbolTags::default();
    let mut needed: Vec<u64> = Vec::new();
    let mut soname: Option<u64> = None;
    let mut terminated = false;
    let entries = table.as_chunks::<DYN_SIZE>().0;
    let mut null_last = false;
    for (index, raw) in entries.iter().enumerate() {
        let d = Fields {
            bytes: raw,
            what: "dynamic table",
        };
        let tag = d.u64(0)?;
        let value = d.u64(8)?;
        match tag {
            dt::NULL => {
                terminated = true;
                null_last = index.checked_add(1) == Some(entries.len());
                break;
            }
            dt::NEEDED => match linking {
                Linking::Importing { .. } => needed.push(value),
                Linking::Static | Linking::Library => return Err(ElfError::Needed),
            },
            // Only a library has a name of its own; anywhere else the tag is outside the
            // contract, as it was before a library could be read.
            dt::SONAME if linking == Linking::Library => {
                if soname.replace(value).is_some() {
                    return Err(ElfError::Soname("it appears twice"));
                }
            }
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
            // The symbol table is read to refuse an undefined symbol no declared library
            // exports, and DT_HASH only for the table's length. The string table is read only
            // where a name is needed: a library's exports and its own name, and a program's
            // imports. The GNU hash table, DT_FLAGS, DT_FLAGS_1 and DT_DEBUG carry nothing an
            // image needs.
            dt::SYMTAB | dt::SYMENT | dt::HASH | dt::STRTAB | dt::STRSZ => {
                symbols.set(tag, value)?;
            }
            dt::GNU_HASH | dt::FLAGS | dt::FLAGS_1 | dt::DEBUG => {}
            other => return Err(ElfError::UnknownDynamicTag(other)),
        }
    }
    if !terminated {
        return Err(ElfError::DynamicTable("no DT_NULL terminator"));
    }

    let table = read_symbols(file, &symbols, segments)?;
    let names = match linking {
        Linking::Static => None,
        Linking::Importing { .. } | Linking::Library => {
            Some(Names::read(file, &symbols, segments)?)
        }
    };
    let mut exports = Vec::new();
    let mut undefined = false;
    // Entry 0 is the reserved null symbol, which is undefined by definition.
    for (index, symbol) in (0u64..).zip(&table).skip(1) {
        let nameless = ElfError::SymbolName { index };
        match (symbol.shndx == SHN_UNDEF, linking, &names) {
            (true, Linking::Importing { exports, .. }, Some(names)) => {
                if !exports.contains(&names.at(symbol.name).ok_or(nameless)?) {
                    return Err(ElfError::UndefinedSymbol { index });
                }
                undefined = true;
            }
            (true, _, _) => return Err(ElfError::UndefinedSymbol { index }),
            (false, Linking::Library, Some(names)) => exports.push(Export {
                name: names.at(symbol.name).ok_or(nameless)?.to_string(),
                value: symbol.value,
                size: symbol.size,
                function: symbol.info & 0xF == STT_FUNC,
                public: symbol.info >> 4 == STB_GLOBAL && symbol.other & 3 == 0,
            }),
            // What a program defines for others is not bound by anything.
            (false, _, _) => {}
        }
    }
    let soname = match (soname, &names) {
        (Some(at), Some(names)) => Some(
            u32::try_from(at)
                .ok()
                .and_then(|at| names.at(at))
                .filter(|name| !name.is_empty())
                .ok_or(ElfError::Soname("it does not name a string of DT_STRTAB"))?
                .to_string(),
        ),
        _ => None,
    };
    if let (Linking::Importing { library, .. }, Some(names)) = (linking, &names) {
        match needed.as_slice() {
            // A program that leaves a symbol to the library must say it needs the library:
            // with no `DT_NEEDED` nothing in the file says where those symbols come from
            // (the review of 2026-11-13, finding F17 b).
            [] if undefined => return Err(ElfError::NeededMissing),
            [] => {}
            [at] => {
                let named = u32::try_from(*at).ok().and_then(|at| names.at(at));
                if named != Some(library) {
                    return Err(ElfError::NeededOther);
                }
            }
            _ => return Err(ElfError::NeededOther),
        }
    }

    let (rela, imports) = read_rela(file, &tags, segments, &table, names.as_ref(), linking)?;
    let relr = read_relr(file, &tags, segments)?;
    let relocations = merge(rela, relr)?;
    // An import slot takes its export's address and nothing else: it is no relocation slot.
    for import in &imports {
        if relocations
            .binary_search_by_key(&import.slot, |r| r.slot)
            .is_ok()
        {
            return Err(ElfError::RelocationConflict { slot: import.slot });
        }
    }
    Ok(Dynamic {
        relocations,
        imports,
        exports,
        soname,
        null_last,
    })
}

/// The symbol- and string-related dynamic tags, each at most once.
#[derive(Default)]
struct SymbolTags {
    symtab: Option<u64>,
    syment: Option<u64>,
    hash: Option<u64>,
    strtab: Option<u64>,
    strsz: Option<u64>,
}

impl SymbolTags {
    fn set(&mut self, tag: u64, value: u64) -> Result<(), ElfError> {
        let field = match tag {
            dt::SYMTAB => &mut self.symtab,
            dt::SYMENT => &mut self.syment,
            dt::HASH => &mut self.hash,
            dt::STRTAB => &mut self.strtab,
            _ => &mut self.strsz,
        };
        if field.replace(value).is_some() {
            return Err(ElfError::DuplicateDynamicTag(tag));
        }
        Ok(())
    }
}

/// One entry of the dynamic symbol table.
#[derive(Clone, Copy)]
struct Symbol {
    /// `st_name`: where its name starts in `DT_STRTAB`.
    name: u32,
    /// `st_info`: its binding and type.
    info: u8,
    /// `st_other`: its visibility.
    other: u8,
    /// `st_shndx`: [`SHN_UNDEF`] if the image does not define it.
    shndx: u16,
    value: u64,
    size: u64,
}

/// The dynamic symbol table's entries, the reserved first one included. Its length is `nchain`,
/// the second word of `DT_HASH` (gABI: "the number of symbol table entries"). An image with no
/// `DT_SYMTAB` has none.
///
/// A limit (S4-1's review, finding F9): the length is the file's own word, so a file can
/// understate it and a symbol past it is not seen. Nothing can use one: a relocation that names
/// a symbol must name one inside the table read here, and every other relocation the
/// conversions accept is `R_X86_64_RELATIVE`, which names none.
fn read_symbols(
    file: &[u8],
    tags: &SymbolTags,
    segments: &[LoadSegment],
) -> Result<Vec<Symbol>, ElfError> {
    let Some(symtab) = tags.symtab else {
        return Ok(Vec::new());
    };
    if tags.syment != Some(SYM_SIZE) {
        return Err(ElfError::SymbolTable("DT_SYMENT is not 24"));
    }
    let hash = tags.hash.ok_or(ElfError::SymbolTable(
        "DT_SYMTAB without DT_HASH, which gives its length",
    ))?;
    let words = table_bytes(file, segments, hash, 8, "DT_HASH")
        .map_err(|_| ElfError::SymbolTable("DT_HASH is outside the file-backed bytes"))?;
    let count = Fields {
        bytes: words,
        what: "DT_HASH",
    }
    .u32(4)?;
    let size = u64::from(count)
        .checked_mul(SYM_SIZE)
        .ok_or(ElfError::SymbolTable("too many symbols"))?;
    let table = table_bytes(file, segments, symtab, size, "DT_SYMTAB")
        .map_err(|_| ElfError::SymbolTable("DT_SYMTAB is outside the file-backed bytes"))?;
    let mut out = Vec::with_capacity(table.len() / SYM_BYTES);
    for raw in table.as_chunks::<SYM_BYTES>().0 {
        let s = Fields {
            bytes: raw,
            what: "DT_SYMTAB",
        };
        let [info] = s.array(SYM_INFO_AT)?;
        let [other] = s.array(SYM_OTHER_AT)?;
        out.push(Symbol {
            name: s.u32(SYM_NAME_AT)?,
            info,
            other,
            shndx: s.u16(SYM_SHNDX_AT)?,
            value: s.u64(SYM_VALUE_AT)?,
            size: s.u64(SYM_SIZE_AT)?,
        });
    }
    Ok(out)
}

/// The dynamic string table: `DT_STRSZ` bytes at `DT_STRTAB`.
struct Names<'a> {
    bytes: &'a [u8],
}

impl<'a> Names<'a> {
    fn read(file: &'a [u8], tags: &SymbolTags, segments: &[LoadSegment]) -> Result<Self, ElfError> {
        let (Some(strtab), Some(strsz)) = (tags.strtab, tags.strsz) else {
            return Err(ElfError::SymbolTable(
                "DT_STRTAB and DT_STRSZ are needed to read a name",
            ));
        };
        let bytes = table_bytes(file, segments, strtab, strsz, "DT_STRTAB")
            .map_err(|_| ElfError::SymbolTable("DT_STRTAB is outside the file-backed bytes"))?;
        Ok(Self { bytes })
    }

    /// The string that starts `at` bytes in: up to its terminator, which must be inside the
    /// table, and UTF-8.
    fn at(&self, at: u32) -> Option<&'a str> {
        let rest = self.bytes.get(usize::try_from(at).ok()?..)?;
        let end = rest.iter().position(|&b| b == 0)?;
        core::str::from_utf8(rest.get(..end)?).ok()
    }
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

/// What the `DT_RELA` table gave: its relative entries as `(slot, addend)`, and its imports.
type RelaEntries = (Vec<(u64, u64)>, Vec<Import>);

/// The `DT_RELA` table's relative entries as `(slot, addend)`, sorted by slot, with exact
/// duplicates removed; and, for a program read as [`Linking::Importing`], its
/// `R_X86_64_GLOB_DAT` entries as import slots, strictly ascending.
fn read_rela(
    file: &[u8],
    tags: &RelocationTags,
    segments: &[LoadSegment],
    symbols: &[Symbol],
    names: Option<&Names<'_>>,
    linking: Linking<'_>,
) -> Result<RelaEntries, ElfError> {
    let (rva, size) = match (tags.rela, tags.relasz, tags.relaent) {
        (None, None, None) => {
            if tags.relacount.is_some() {
                return Err(ElfError::RelocationTags("DT_RELACOUNT without DT_RELA"));
            }
            return Ok((Vec::new(), Vec::new()));
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
    let mut imports: Vec<Import> = Vec::new();
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
        if let (R_X86_64_GLOB_DAT, Linking::Importing { .. }, Some(names)) =
            (r_type, linking, names)
        {
            // It names an undefined symbol of the table, which the pass over the table has
            // already held to the library's exports; the reserved entry 0 is none.
            let named = usize::try_from(symbol)
                .ok()
                .filter(|&index| index != 0)
                .and_then(|index| symbols.get(index))
                .filter(|s| s.shndx == SHN_UNDEF)
                .ok_or(ElfError::ImportSymbol { slot, symbol })?;
            if addend != 0 {
                return Err(ElfError::ImportAddend { slot });
            }
            check_slot(slot, segments)?;
            let name = names.at(named.name).ok_or(ElfError::SymbolName {
                index: u64::from(symbol),
            })?;
            imports.push(Import {
                slot,
                symbol: name.to_string(),
            });
            continue;
        }
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
    // One slot, one export: two imports at a slot would leave it to the order they are bound.
    imports.sort_by_key(|import| import.slot);
    for pair in imports.windows(2) {
        if let [a, b] = pair
            && a.slot == b.slot
        {
            return Err(ElfError::RelocationConflict { slot: a.slot });
        }
    }
    Ok((out, imports))
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
