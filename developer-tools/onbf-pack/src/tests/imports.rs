//! The reading of a program that imports from one named library (`elf::Linking::Importing`;
//! task S4-2, ADR-0011 Amendment 2, L3): what it accepts, each thing it refuses, and that the
//! reading of a static image refuses all of it, as before.

use super::elf_writer::{Load, Phdr, R, RELATIVE, RW, Sym, TestElf, symbol_tables};
use crate::elf::{ElfError, Import, Linking, Permissions, SlotProblem, parse, parse_linked};

const LIBRARY: &str = "origin.core";
const EXPORTS: &[&str] = &["origin_kernel_a", "origin_kernel_b", "origin_kernel_c"];
/// `R_X86_64_GLOB_DAT`, for `r_info`'s low half.
const GLOB_DAT: u64 = 6;

fn importing() -> Linking<'static> {
    Linking::Importing {
        library: LIBRARY,
        exports: EXPORTS,
    }
}

/// `r_info` of a `GLOB_DAT` relocation against symbol `index`.
const fn glob_dat(index: u64) -> u64 {
    index << 32 | GLOB_DAT
}

/// A program that imports `origin_kernel_b` (symbol 1) and `origin_kernel_a` (symbol 2), with a
/// slot for each in its RELRO page at 0x3000, a relative relocation beside them, and a
/// `DT_NEEDED` that names the library. Its tables are at 0x5000.
fn program(edit: impl FnOnce(&mut Vec<Sym>, &mut TestElf)) -> Vec<u8> {
    let mut symbols = vec![
        Sym::undefined("origin_kernel_b"),
        Sym::undefined("origin_kernel_a"),
    ];
    let mut elf = TestElf::kernel();
    elf.loads[2] = Load::new(RW, 0x3000, vec![0; 64]).mem(0x1000);
    elf.extra.push(Phdr::relro(0x3000, 0x1000));
    elf.rela = vec![
        (0x3000, RELATIVE, 0x2008),
        (0x3010, glob_dat(1), 0),
        (0x3018, glob_dat(2), 0),
    ];
    edit(&mut symbols, &mut elf);
    let (tables, mut tags, at) = symbol_tables(0x5000, &symbols, &[LIBRARY, "origin.other"]);
    elf.loads.push(Load::new(R, 0x5000, tables));
    // Every `DT_NEEDED` the edit asked for by a placeholder gets its string's place.
    for tag in &mut elf.dynamic {
        if tag.0 == 1 {
            tag.1 = at[tag.1 as usize];
        }
    }
    if !elf.dynamic.iter().any(|&(tag, _)| tag == 1) {
        tags.push((1, at[0]));
    }
    elf.dynamic.extend(tags);
    elf.bytes()
}

#[test]
fn an_importing_program_gives_its_import_slots_by_name() {
    let elf = parse_linked(&program(|_, _| {}), importing()).expect("it reads");
    assert_eq!(
        elf.imports,
        [
            Import {
                slot: 0x3010,
                symbol: "origin_kernel_b".to_string()
            },
            Import {
                slot: 0x3018,
                symbol: "origin_kernel_a".to_string()
            },
        ]
    );
    // The relative relocation is still one, and the import slots are none.
    let slots: Vec<u64> = elf.relocations.iter().map(|r| r.slot).collect();
    assert_eq!(slots, [0x3000]);
    // Both lie in the page that is read-only once relocated.
    let relro = elf
        .segments
        .iter()
        .find(|s| s.relro)
        .expect("a RELRO segment");
    assert_eq!(
        (relro.vaddr, relro.permissions),
        (0x3000, Permissions::ReadWrite)
    );
    assert!(elf.exports.is_empty() && elf.soname.is_none());
    // A program that needs nothing and imports nothing reads the same way.
    let plain = program(|symbols, elf| {
        symbols.clear();
        elf.rela.truncate(1);
    });
    assert!(
        parse_linked(&plain, importing())
            .unwrap()
            .imports
            .is_empty()
    );
}

#[test]
fn a_static_reading_refuses_every_part_of_an_import() {
    // The whole program: its `DT_NEEDED` comes first.
    assert_eq!(parse(&program(|_, _| {})), Err(ElfError::Needed));
    // With no `DT_NEEDED`: the undefined symbol.
    let mut elf = TestElf::kernel();
    elf.rela = vec![(0x3010, glob_dat(1), 0)];
    let (tables, tags, _) = symbol_tables(0x5000, &[Sym::undefined("origin_kernel_b")], &[]);
    elf.loads.push(Load::new(R, 0x5000, tables));
    elf.dynamic.extend(tags);
    assert_eq!(
        parse(&elf.bytes()),
        Err(ElfError::UndefinedSymbol { index: 1 })
    );
    // With the symbol defined, the relocation's type is what is left to refuse.
    let mut elf = TestElf::kernel();
    elf.rela = vec![(0x3010, glob_dat(1), 0)];
    let (tables, tags, _) =
        symbol_tables(0x5000, &[Sym::function("origin_kernel_b", 0x1000, 4)], &[]);
    elf.loads.push(Load::new(R, 0x5000, tables));
    elf.dynamic.extend(tags);
    assert_eq!(
        parse(&elf.bytes()),
        Err(ElfError::UnsupportedRelocation {
            slot: 0x3010,
            r_type: 6
        })
    );
}

#[test]
fn only_the_declared_library_may_be_needed() {
    // Another library's name.
    let other = program(|_, elf| elf.dynamic.push((1, 1)));
    assert_eq!(
        parse_linked(&other, importing()),
        Err(ElfError::NeededOther)
    );
    // The right one, twice.
    let twice = program(|_, elf| elf.dynamic.extend([(1, 0), (1, 0)]));
    assert_eq!(
        parse_linked(&twice, importing()),
        Err(ElfError::NeededOther)
    );
    // The right one and another.
    let both = program(|_, elf| elf.dynamic.extend([(1, 0), (1, 1)]));
    assert_eq!(parse_linked(&both, importing()), Err(ElfError::NeededOther));
    // A program is no library: it has no name of its own.
    let named = program(|_, elf| elf.dynamic.push((14, 1)));
    assert_eq!(
        parse_linked(&named, importing()),
        Err(ElfError::UnknownDynamicTag(14))
    );
}

#[test]
fn an_undefined_symbol_the_library_does_not_export_is_refused() {
    let unknown = program(|symbols, _| symbols.push(Sym::undefined("origin_kernel_helper")));
    assert_eq!(
        parse_linked(&unknown, importing()),
        Err(ElfError::UndefinedSymbol { index: 3 })
    );
    // Whether or not anything is bound to it.
    let unbound = program(|symbols, elf| {
        symbols[0] = Sym::undefined("memcpy");
        elf.rela.truncate(1);
    });
    assert_eq!(
        parse_linked(&unbound, importing()),
        Err(ElfError::UndefinedSymbol { index: 1 })
    );
}

#[test]
fn an_import_must_name_an_undefined_symbol_of_the_table_with_no_addend() {
    let refused = |edit: fn(&mut Vec<Sym>, &mut TestElf), expected: ElfError| {
        assert_eq!(parse_linked(&program(edit), importing()), Err(expected));
    };
    // No symbol at all, one past the table's end, and one the program defines itself.
    refused(
        |_, elf| elf.rela[1].1 = glob_dat(0),
        ElfError::ImportSymbol {
            slot: 0x3010,
            symbol: 0,
        },
    );
    refused(
        |_, elf| elf.rela[1].1 = glob_dat(3),
        ElfError::ImportSymbol {
            slot: 0x3010,
            symbol: 3,
        },
    );
    refused(
        |symbols, elf| {
            symbols.push(Sym::function("local_function", 0x1000, 4));
            elf.rela[1].1 = glob_dat(3);
        },
        ElfError::ImportSymbol {
            slot: 0x3010,
            symbol: 3,
        },
    );
    refused(
        |_, elf| elf.rela[2].2 = 8,
        ElfError::ImportAddend { slot: 0x3018 },
    );
    // `R_X86_64_JUMP_SLOT` is not read: the build writes none.
    refused(
        |_, elf| elf.rela[1].1 = 1 << 32 | 7,
        ElfError::UnsupportedRelocation {
            slot: 0x3010,
            r_type: 7,
        },
    );
    // Nor are the tags of a procedure linkage table (`DT_JMPREL`).
    refused(
        |_, elf| elf.dynamic.push((23, 0x5000)),
        ElfError::UnknownDynamicTag(23),
    );
}

#[test]
fn an_import_slot_is_held_to_the_rules_of_a_relocation_slot_and_is_none() {
    let refused = |edit: fn(&mut Vec<Sym>, &mut TestElf), expected: ElfError| {
        assert_eq!(parse_linked(&program(edit), importing()), Err(expected));
    };
    refused(
        |_, elf| elf.rela[1].0 = 0x3014,
        ElfError::RelocationSlot {
            slot: 0x3014,
            problem: SlotProblem::Misaligned,
        },
    );
    refused(
        |_, elf| elf.rela[1].0 = 0x2008,
        ElfError::RelocationSlot {
            slot: 0x2008,
            problem: SlotProblem::NotWritable(Permissions::Read),
        },
    );
    refused(
        |_, elf| elf.rela[1].0 = 0x1008,
        ElfError::RelocationSlot {
            slot: 0x1008,
            problem: SlotProblem::NotWritable(Permissions::ReadExecute),
        },
    );
    refused(
        |_, elf| elf.rela[1].0 = 0x3800,
        ElfError::RelocationSlot {
            slot: 0x3800,
            problem: SlotProblem::Bss,
        },
    );
    // Two imports at one slot, and an import at a relative relocation's slot.
    refused(
        |_, elf| elf.rela[2].0 = 0x3010,
        ElfError::RelocationConflict { slot: 0x3010 },
    );
    refused(
        |_, elf| elf.rela[1].0 = 0x3000,
        ElfError::RelocationConflict { slot: 0x3000 },
    );
}

#[test]
fn a_name_that_cannot_be_read_is_refused() {
    // No string table to read a name from.
    let mut elf = TestElf::kernel();
    let (tables, mut tags, _) = symbol_tables(0x5000, &[Sym::undefined("origin_kernel_b")], &[]);
    tags.retain(|&(tag, _)| tag != 5);
    elf.loads.push(Load::new(R, 0x5000, tables));
    elf.dynamic.extend(tags);
    assert_eq!(
        parse_linked(&elf.bytes(), importing()),
        Err(ElfError::SymbolTable(
            "DT_STRTAB and DT_STRSZ are needed to read a name"
        ))
    );
    // A string table cut short of the name's terminator.
    let mut elf = TestElf::kernel();
    let (tables, mut tags, _) = symbol_tables(0x5000, &[Sym::undefined("origin_kernel_b")], &[]);
    for tag in &mut tags {
        if tag.0 == 10 {
            tag.1 = 4;
        }
    }
    elf.loads.push(Load::new(R, 0x5000, tables));
    elf.dynamic.extend(tags);
    assert_eq!(
        parse_linked(&elf.bytes(), importing()),
        Err(ElfError::SymbolName { index: 1 })
    );
    // The string table named twice.
    let mut elf = TestElf::kernel();
    let (tables, mut tags, _) = symbol_tables(0x5000, &[], &[]);
    tags.push((5, 0x5000));
    elf.loads.push(Load::new(R, 0x5000, tables));
    elf.dynamic.extend(tags);
    assert_eq!(
        parse_linked(&elf.bytes(), importing()),
        Err(ElfError::DuplicateDynamicTag(5))
    );
}

// ------------------------------------------------------------------ the description

use crate::carried::{CarriedError, describe, describe_importing};
use crate::corelib::Listing;

/// The generator's list for a library that exports [`EXPORTS`], in number order, which is not
/// name order.
fn listing() -> Listing {
    Listing::parse(
        "# oidl-core-exports 1 origin.kernel\n\
         release sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\n\
         0 origin_kernel_c c 0.0\n1 origin_kernel_a a 0.0\n2 origin_kernel_b b 0.5\n",
    )
    .expect("the list reads")
}

#[test]
fn a_programs_imports_are_laid_out_as_slots_and_exports_by_name_order() {
    let carried = describe_importing(&program(|_, _| {}), &listing()).expect("it converts");
    let imports: Vec<(u64, usize, &str)> = carried
        .imports
        .iter()
        .map(|i| (i.slot, i.export, i.symbol.as_str()))
        .collect();
    // `a` is export 0 and `b` export 1, whatever their call numbers.
    assert_eq!(
        imports,
        [
            (0x3010, 1, "origin_kernel_b"),
            (0x3018, 0, "origin_kernel_a")
        ]
    );
    assert_eq!(carried.relocations, [0x3000]);
    assert_eq!(carried.core.map(|release| release[1]), Some(0x11));
    // The description holds zero in each import slot: its value is the launch path's to write.
    let relro = carried
        .segments
        .iter()
        .find(|s| s.rva == 0x3000)
        .expect("the RELRO segment");
    assert_eq!(relro.access, crate::carried::Access::ReadAfterRelocation);
    assert_eq!(relro.bytes[0x10..0x20], [0; 16]);
    // The relative relocation's addend is in place beside them.
    assert_eq!(relro.bytes[..8], 0x2008u64.to_le_bytes());
    let files = carried.rust("importer", "ORIGIN_CARRIED").unwrap();
    let text = String::from_utf8(files[0].1.clone()).unwrap();
    for line in [
        "    imports: &[",
        "        Import { slot: 0x3010, export: 1 }, // origin_kernel_b",
        "        Import { slot: 0x3018, export: 0 }, // origin_kernel_a",
        "    core: Some([",
        "        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,",
    ] {
        assert!(
            text.contains(&format!("{line}\n")),
            "no line `{line}` in:\n{text}"
        );
    }
    assert_eq!(
        files,
        describe_importing(&program(|_, _| {}), &listing())
            .unwrap()
            .rust("importer", "ORIGIN_CARRIED")
            .unwrap(),
        "the same twice"
    );
}

#[test]
fn a_slot_left_in_the_import_by_the_linker_is_zero_in_the_description() {
    // A linker may leave anything in a slot it expects a loader to fill.
    let bytes = program(|_, elf| elf.loads[2].data[0x18..0x20].copy_from_slice(&[0xAA; 8]));
    let carried = describe_importing(&bytes, &listing()).unwrap();
    let relro = carried.segments.iter().find(|s| s.rva == 0x3000).unwrap();
    assert_eq!(relro.bytes[0x18..0x20], [0; 8]);
}

#[test]
fn an_import_slot_outside_relro_is_refused() {
    // The same program with no `PT_GNU_RELRO`: its slots are in plain writable data, which the
    // program could write over.
    let writable = program(|_, elf| elf.extra.retain(|header| header.p_type != 0x6474_E552));
    assert_eq!(
        describe_importing(&writable, &listing()),
        Err(CarriedError::ImportSlot(0x3010))
    );
}

#[test]
fn a_program_that_imports_is_refused_when_no_library_is_named() {
    assert_eq!(
        describe(&program(|_, _| {})),
        Err(CarriedError::Elf(ElfError::Needed))
    );
    // And a program that imports nothing says so in its description.
    let plain = describe(&TestElf::kernel().bytes()).unwrap();
    assert!(plain.imports.is_empty() && plain.core.is_none());
    let text =
        String::from_utf8(plain.rust("plain", "ORIGIN_CARRIED").unwrap()[0].1.clone()).unwrap();
    assert!(
        text.contains("    imports: &[\n    ],\n    core: None,\n};\n"),
        "{text}"
    );
}

#[test]
fn an_import_of_a_symbol_the_list_does_not_name_is_refused_by_the_reader() {
    // The list is where the reader's exports come from, so a symbol it lacks never becomes an
    // import: the reader refuses the undefined symbol first.
    let short = Listing::parse(
        "# oidl-core-exports 1 origin.kernel\n\
         release sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\n\
         0 origin_kernel_a a 0.0\n",
    )
    .unwrap();
    assert_eq!(
        describe_importing(&program(|_, _| {}), &short),
        Err(CarriedError::Elf(ElfError::UndefinedSymbol { index: 1 }))
    );
}
