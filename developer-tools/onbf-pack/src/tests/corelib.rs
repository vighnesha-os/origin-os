//! `corelib`: `origin.core` laid out for the kernel (task S4-2; ADR-0011 Amendment 2, L2 and L4
//! to L7). A library made by hand converts, to the same files twice; and each thing that would
//! make it more than the listed stubs, or less, is refused, one fixture each.

use super::elf_writer::{
    Load, PT_DYNAMIC, PT_LOAD, R, RELATIVE, RW, RX, RawElf, Sym, TestElf, symbol_tables,
};
use crate::corelib::{CoreError, LIBRARY, Listing, describe};
use crate::elf::ElfError;

/// A stub as the generator writes one with no narrow parameter: `mov eax, number`, `syscall`,
/// `ret`.
fn stub(number: u32) -> Vec<u8> {
    let mut bytes = vec![0xB8];
    bytes.extend_from_slice(&number.to_le_bytes());
    bytes.extend_from_slice(&[0x0F, 0x05, 0xC3]);
    bytes
}

/// A library made by hand: its code at 0x1000, its tables (hash, symbols, strings) at 0x2000,
/// and the dynamic table in a read-only segment after them, as [`TestElf`] places it.
#[derive(Clone)]
struct Library {
    code: Vec<u8>,
    symbols: Vec<Sym>,
    soname: Option<&'static str>,
}

impl Library {
    /// Three stubs, each padded with `int3` to a multiple of four bytes, exported under names
    /// that do not sort in number order.
    fn good() -> Self {
        let mut library = Self {
            code: Vec::new(),
            symbols: Vec::new(),
            soname: Some(LIBRARY),
        };
        for (name, number) in [
            ("origin_kernel_b", 0),
            ("origin_kernel_c", 1),
            ("origin_kernel_a", 2),
        ] {
            library.push(name, &stub(number));
        }
        library
    }

    /// Appends `bytes` as a global function named `name`, then padding to four bytes.
    fn push(&mut self, name: &'static str, bytes: &[u8]) {
        let value = 0x1000 + self.code.len() as u64;
        self.code.extend_from_slice(bytes);
        while !self.code.len().is_multiple_of(4) {
            self.code.push(0xCC);
        }
        self.symbols
            .push(Sym::function(name, value, bytes.len() as u64));
    }

    fn elf(&self) -> TestElf {
        let strings: Vec<&str> = self.soname.into_iter().collect();
        let (tables, mut dynamic, at) = symbol_tables(0x2000, &self.symbols, &strings);
        if let Some(&soname) = at.first() {
            dynamic.insert(0, (14, soname));
        }
        let mut elf = TestElf::kernel();
        elf.entry = 0;
        elf.loads = vec![
            Load::new(RX, 0x1000, self.code.clone()),
            Load::new(R, 0x2000, tables),
        ];
        elf.dynamic = dynamic;
        elf
    }

    fn bytes(&self) -> Vec<u8> {
        self.elf().bytes()
    }
}

const RELEASE: &str =
    "release sha256:00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// The generator's list for [`Library::good`]: three calls, in number order.
fn listing() -> Listing {
    Listing::parse(&format!(
        "# oidl-core-exports 1 origin.kernel\n{RELEASE}\n\
         0 origin_kernel_b b 0.0\n1 origin_kernel_c c 0.5\n2 origin_kernel_a a 1.2\n"
    ))
    .expect("the list reads")
}

#[track_caller]
fn refuses(library: &Library, expected: CoreError) {
    assert_eq!(describe(&library.bytes(), &listing()), Err(expected));
}

#[test]
fn a_library_of_listed_stubs_is_laid_out_with_its_exports_and_pins() {
    let core = describe(&Library::good().bytes(), &listing()).expect("it converts");
    assert_eq!(core.span, 0x1000);
    // Three stubs of eight bytes: no padding between them.
    assert_eq!(core.code.len(), 24);
    assert_eq!(core.code[..8], stub(0));
    // Exports by name, each with its place and its method's version.
    let exports: Vec<(&str, u64, (u16, u16))> = core
        .exports
        .iter()
        .map(|x| (x.name.as_str(), x.offset, x.since))
        .collect();
    assert_eq!(
        exports,
        [
            ("origin_kernel_a", 16, (1, 2)),
            ("origin_kernel_b", 0, (0, 0)),
            ("origin_kernel_c", 8, (0, 5)),
        ]
    );
    // Pins by number: each stub's `syscall` is five bytes in, after the load of its number.
    let pins: Vec<(&str, u64, u64)> = core
        .pins
        .iter()
        .map(|p| (p.method.as_str(), p.number, p.offset))
        .collect();
    assert_eq!(pins, [("b", 0, 5), ("c", 1, 13), ("a", 2, 21)]);
    for pin in &core.pins {
        assert_eq!(core.code[pin.offset as usize..][..2], [0x0F, 0x05]);
    }
    assert_eq!(core.release[..4], [0x00, 0x11, 0x22, 0x33]);
    // An import is written as its export's place among the names in order.
    assert_eq!(listing().export_index("origin_kernel_a"), Some(0));
    assert_eq!(listing().export_index("origin_kernel_c"), Some(2));
    assert_eq!(listing().export_index("origin_kernel_d"), None);
}

#[test]
fn the_description_is_the_kernels_source_and_the_same_twice() {
    let core = describe(&Library::good().bytes(), &listing()).unwrap();
    let files = core.rust("ORIGIN_CARRIED").unwrap();
    assert_eq!(
        files,
        describe(&Library::good().bytes(), &listing())
            .unwrap()
            .rust("ORIGIN_CARRIED")
            .unwrap()
    );
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["origin_core.rs", "origin_core.0.bin"]);
    assert_eq!(files[1].1, core.code);
    let text = String::from_utf8(files[0].1.clone()).unwrap();
    assert!(
        text.starts_with(&crate::corelib::generated_line()),
        "{text}"
    );
    for line in [
        "pub(crate) static ORIGIN_CORE: CoreImage = CoreImage {",
        "    span: 0x1000,",
        "    code: include_bytes!(concat!(env!(\"ORIGIN_CARRIED\"), \"/origin_core.0.bin\")),",
        "        CoreExport { name: \"origin_kernel_a\", offset: 0x10, since: (1, 2) },",
        "        CoreExport { name: \"origin_kernel_b\", offset: 0x0, since: (0, 0) },",
        "    pins: Pins {",
        "        b: 0x5,",
        "        c: 0xd,",
        "        a: 0x15,",
        "        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,",
    ] {
        assert!(
            text.contains(&format!("{line}\n")),
            "no line `{line}` in:\n{text}"
        );
    }
    // Nothing of the environment: no path, and the variable only by name.
    let paths = text
        .lines()
        .filter(|line| !line.starts_with("//") && !line.contains("include_bytes!"))
        .filter(|line| line.contains('/'))
        .count();
    assert_eq!(paths, 0, "{text}");
    assert!(core.rust("not a variable").is_err());
}

#[test]
fn a_stub_with_a_second_syscall_or_none_is_refused() {
    // Two `syscall`s for one number: the second could be reached with another number in RAX.
    let mut twice = Library::good();
    let mut bytes = stub(3);
    bytes.splice(7..7, [0x0F, 0x05]);
    twice.push("origin_kernel_d", &bytes);
    let list = Listing::parse(&format!(
        "# oidl-core-exports 1 origin.kernel\n{RELEASE}\n\
         0 origin_kernel_b b 0.0\n1 origin_kernel_c c 0.5\n2 origin_kernel_a a 1.2\n3 origin_kernel_d d 0.0\n"
    ))
    .unwrap();
    assert_eq!(
        describe(&twice.bytes(), &list),
        Err(CoreError::Syscalls {
            name: "origin_kernel_d".to_string(),
            found: 2
        })
    );
    // The pair inside another instruction counts too: `mov eax, 0x050f` holds it.
    let mut hidden = Library::good();
    hidden.code[1..5].copy_from_slice(&0x050Fu32.to_le_bytes());
    refuses(
        &hidden,
        CoreError::Syscalls {
            name: "origin_kernel_b".to_string(),
            found: 2,
        },
    );
    // None at all.
    let mut none = Library::good();
    none.code[5..7].copy_from_slice(&[0x90, 0x90]);
    refuses(
        &none,
        CoreError::Syscalls {
            name: "origin_kernel_b".to_string(),
            found: 0,
        },
    );
}

#[test]
fn a_syscall_or_any_other_byte_outside_every_stub_is_refused() {
    // After the last stub, where no symbol covers it.
    let mut after = Library::good();
    after.code.extend_from_slice(&[0x0F, 0x05, 0xC3, 0xCC]);
    refuses(&after, CoreError::StraySyscall(24));
    // In what would be padding between two stubs.
    let mut between = Library::good();
    between.symbols[0].size = 6;
    between.code[6..8].copy_from_slice(&[0xCC, 0xCC]);
    // Stub `b` now ends before its `syscall`: it holds none.
    refuses(
        &between,
        CoreError::Syscalls {
            name: "origin_kernel_b".to_string(),
            found: 0,
        },
    );
    // A function nothing exports: its bytes belong to no stub.
    let mut stray = Library::good();
    stray.code.extend_from_slice(&[0x31, 0xC0, 0xC3, 0xCC]);
    refuses(&stray, CoreError::Stray(24));
    // Padding that is `int3` is what the linker writes, and is accepted.
    let mut padded = Library::good();
    padded.code.extend_from_slice(&[0xCC; 8]);
    assert!(describe(&padded.bytes(), &listing()).is_ok());
}

#[test]
fn a_syscall_across_two_adjacent_stubs_is_refused() {
    // The first stub ends in `0F` and the second starts with `05`, with no padding between
    // them. Each holds its own pair once, and the pair across their boundary is in neither
    // (the review of 2026-11-13, finding F6 a).
    let mut across = Library {
        code: Vec::new(),
        symbols: Vec::new(),
        soname: Some(LIBRARY),
    };
    across.push("origin_kernel_b", &[0xB8, 0, 0, 0, 0, 0x0F, 0x05, 0x0F]);
    across.push("origin_kernel_c", &[0x05, 0x0F, 0x05, 0xC3]);
    across.push("origin_kernel_a", &stub(2));
    assert_eq!(across.code[7..9], [0x0F, 0x05]);
    refuses(&across, CoreError::StraySyscall(7));
}

/// [`Library::good`] with its dynamic table in a writable segment of its own, which is where
/// the linker really puts it, changed by `edit`. Returns the file and that segment's address.
fn with_writable_table(edit: impl FnOnce(&mut RawElf, usize, usize)) -> (Vec<u8>, u64) {
    let mut raw = Library::good().elf().raw();
    let load = raw
        .phdrs
        .iter()
        .rposition(|p| p.p_type == PT_LOAD)
        .expect("the table's segment");
    let table = raw
        .phdrs
        .iter()
        .position(|p| p.p_type == PT_DYNAMIC)
        .expect("the table");
    // The fixture's writer puts nothing else in that segment: it is the table exactly.
    assert_eq!(
        (raw.phdrs[load].offset, raw.phdrs[load].filesz),
        (raw.phdrs[table].offset, raw.phdrs[table].filesz)
    );
    raw.phdrs[load].flags = RW;
    edit(&mut raw, load, table);
    let vaddr = raw.phdrs[load].vaddr;
    (raw.bytes(), vaddr)
}

#[test]
fn the_one_writable_segment_is_the_dynamic_table_and_zero_fill_to_the_end_of_its_page() {
    // The table alone, writable: what every really linked library has.
    let (alone, _) = with_writable_table(|_, _, _| {});
    assert!(describe(&alone, &listing()).is_ok());
    // With zero-filled memory after it to the end of its page: the linker's padding.
    let (padded, _) = with_writable_table(|raw, load, _| raw.phdrs[load].memsz = 0x1000);
    assert!(describe(&padded, &listing()).is_ok());
    // Memory that runs past the table's page is writable memory of its own.
    let (past, at) = with_writable_table(|raw, load, _| raw.phdrs[load].memsz = 0x1001);
    assert_eq!(
        describe(&past, &listing()),
        Err(CoreError::WritableData(at))
    );
    // A table that starts mid-page could carry zero fill into the next page and pass a rule on
    // its size; the rule is on its end, and the reader refuses such a segment first (finding
    // K13).
    let (mid, _) = with_writable_table(|raw, load, table| {
        for header in [load, table] {
            raw.phdrs[header].vaddr += 0x10;
            raw.phdrs[header].offset += 0x10;
        }
        raw.phdrs[load].memsz += 0x1000;
    });
    assert!(
        matches!(
            describe(&mid, &listing()),
            Err(CoreError::Elf(ElfError::SegmentAlignment { .. }))
        ),
        "{:?}",
        describe(&mid, &listing())
    );
    // The table and eight more bytes: initialised data beside it (finding F6 b).
    let (more, at) = with_writable_table(|raw, load, _| {
        let end = raw.phdrs[load].offset + raw.phdrs[load].filesz;
        raw.put(end, &[1; 8]);
        raw.phdrs[load].filesz += 8;
        raw.phdrs[load].memsz += 8;
    });
    assert_eq!(
        describe(&more, &listing()),
        Err(CoreError::WritableData(at))
    );
}

#[test]
fn nothing_follows_the_dynamic_tables_null() {
    // One more entry after `DT_NULL`, inside `PT_DYNAMIC`: bytes a reader that stops at the
    // null never sees (finding F3). `DT_FLAGS` is a tag the reader takes, so nothing else
    // refuses the file.
    let (after, _) = with_writable_table(|raw, load, table| {
        let end = raw.phdrs[table].offset + raw.phdrs[table].filesz;
        let mut entry = Vec::new();
        entry.extend_from_slice(&30u64.to_le_bytes());
        entry.extend_from_slice(&8u64.to_le_bytes());
        raw.put(end, &entry);
        for header in [load, table] {
            raw.phdrs[header].filesz += 16;
            raw.phdrs[header].memsz += 16;
        }
    });
    assert_eq!(describe(&after, &listing()), Err(CoreError::TableEnd));
    // The same entry before the null is read, and the library converts.
    let mut before = Library::good().elf();
    before.dynamic.push((30, 8));
    assert!(describe(&before.bytes(), &listing()).is_ok());
}

#[test]
fn anything_to_relocate_and_any_writable_data_is_refused() {
    // A writable segment with bytes, which is not the dynamic table.
    let mut writable = Library::good().elf();
    writable.loads.push(Load::new(RW, 0x3000, vec![1; 16]));
    assert_eq!(
        describe(&writable.bytes(), &listing()),
        Err(CoreError::WritableData(0x3000))
    );
    // The same segment with a relocation into it: nothing to relocate comes first.
    let mut relocated = writable.clone();
    relocated.rela = vec![(0x3000, RELATIVE, 0x1000)];
    assert_eq!(
        describe(&relocated.bytes(), &listing()),
        Err(CoreError::Relocations(1))
    );
    // Zero-filled writable memory with no byte in the file: what a `static mut` that starts
    // at zero makes. It is writable data all the same.
    let mut zeroed = Library::good().elf();
    zeroed
        .loads
        .push(Load::new(RW, 0x3000, Vec::new()).mem(0x1000));
    assert_eq!(
        describe(&zeroed.bytes(), &listing()),
        Err(CoreError::WritableData(0x3000))
    );
}

#[test]
fn more_or_less_than_one_code_segment_is_refused() {
    let mut two = Library::good().elf();
    two.loads.push(Load::new(RX, 0x4000, vec![0xCC; 8]));
    assert_eq!(
        describe(&two.bytes(), &listing()),
        Err(CoreError::CodeSegments(2))
    );
    let mut none = Library::good().elf();
    none.loads.remove(0);
    assert_eq!(
        describe(&none.bytes(), &listing()),
        Err(CoreError::CodeSegments(0))
    );
}

#[test]
fn the_exports_are_exactly_the_listed_entry_points() {
    // One more than the list names.
    let mut more = Library::good();
    more.push("origin_kernel_helper", &stub(9));
    refuses(
        &more,
        CoreError::Unlisted("origin_kernel_helper".to_string()),
    );
    // One fewer.
    let mut fewer = Library::good();
    fewer.symbols.pop();
    fewer.code.truncate(16);
    refuses(&fewer, CoreError::Missing("origin_kernel_a".to_string()));
    // One twice.
    let mut twice = Library::good();
    let again = twice.symbols[0].clone();
    twice.symbols.push(again);
    refuses(&twice, CoreError::Missing("origin_kernel_b".to_string()));
}

#[test]
fn an_export_that_is_not_a_public_function_inside_the_code_is_refused() {
    let wrong = |edit: fn(&mut Sym)| {
        let mut library = Library::good();
        edit(&mut library.symbols[1]);
        refuses(&library, CoreError::Export("origin_kernel_c".to_string()));
    };
    wrong(|s| s.info = 0x11); // an object
    wrong(|s| s.info = 0x22); // a weak function
    wrong(|s| s.other = 2); // hidden
    wrong(|s| s.size = 0);
    wrong(|s| s.value = 0x0FF8); // before the code
    wrong(|s| s.size = 0x100); // past its end
    // Two stubs that share bytes.
    let mut overlap = Library::good();
    overlap.symbols[1].value = 0x1004;
    refuses(&overlap, CoreError::Export("origin_kernel_c".to_string()));
}

#[test]
fn a_library_must_be_origin_core_with_no_entry_no_need_and_nothing_undefined() {
    let mut other = Library::good();
    other.soname = Some("origin.other");
    refuses(&other, CoreError::Name("origin.other".to_string()));
    let mut unnamed = Library::good();
    unnamed.soname = None;
    refuses(
        &unnamed,
        CoreError::Elf(ElfError::Soname("a library must have one")),
    );
    let mut entry = Library::good().elf();
    entry.entry = 0x1000;
    assert_eq!(
        describe(&entry.bytes(), &listing()),
        Err(CoreError::Elf(ElfError::LibraryEntry(0x1000)))
    );
    let mut needs = Library::good().elf();
    needs.dynamic.push((1, 1));
    assert_eq!(
        describe(&needs.bytes(), &listing()),
        Err(CoreError::Elf(ElfError::Needed))
    );
    let mut undefined = Library::good();
    undefined.symbols[2].shndx = 0;
    refuses(
        &undefined,
        CoreError::Elf(ElfError::UndefinedSymbol { index: 3 }),
    );
    // A name of its own twice.
    let mut twice = Library::good().elf();
    twice.dynamic.push((14, 1));
    assert_eq!(
        describe(&twice.bytes(), &listing()),
        Err(CoreError::Elf(ElfError::Soname("it appears twice")))
    );
}

#[test]
fn a_static_image_still_may_not_name_itself() {
    // DT_SONAME is read for a library only: for the KRN image and a static program it is a
    // tag outside the contract, as before.
    let mut elf = TestElf::kernel();
    elf.dynamic.push((14, 1));
    assert_eq!(
        crate::elf::parse(&elf.bytes()),
        Err(ElfError::UnknownDynamicTag(14))
    );
}

#[test]
fn the_list_of_exports_is_read_strictly() {
    let good = format!(
        "# oidl-core-exports 1 origin.kernel\n{RELEASE}\n0 origin_kernel_b b 0.0\n7 origin_kernel_c c 0.5\n"
    );
    let list = Listing::parse(&good).unwrap();
    assert_eq!(list.library, "origin.kernel");
    assert_eq!(list.symbols(), ["origin_kernel_b", "origin_kernel_c"]);
    assert_eq!(list.entries[1].number, 7);
    assert_eq!(list.entries[1].since, (0, 5));
    let refused = |text: &str, line: usize| {
        let Err(CoreError::List(why)) = Listing::parse(text) else {
            panic!("accepted: {text}");
        };
        assert!(why.starts_with(&format!("line {line}:")), "{why}");
    };
    refused("", 1);
    refused(&good.replacen("exports 1", "exports 2", 1), 1);
    refused(&good.replacen("origin.kernel", "", 1), 1);
    refused(&good.replacen("sha256:", "sha512:", 1), 2);
    refused(&good.replacen("0011", "001", 1), 2);
    refused(&good.replacen("0011", "00AA", 1), 2);
    refused(
        &format!("# oidl-core-exports 1 origin.kernel\n{RELEASE}\n"),
        3,
    );
    refused(&good.replacen("0 origin", "x origin", 1), 3);
    refused(&good.replacen("0 origin", "4294967296 origin", 1), 3);
    refused(&good.replacen(" b 0.0", " b 0.0 more", 1), 3);
    refused(&good.replacen(" b 0.0", " b", 1), 3);
    refused(&good.replacen(" b 0.0", " b 0", 1), 3);
    refused(&good.replacen(" b 0.0", " B 0.0", 1), 3);
    refused(&good.replacen("origin_kernel_b", "origin\"kernel", 1), 3);
    // Numbers ascend, and no symbol or method is named twice.
    refused(&good.replacen("7 origin", "0 origin", 1), 4);
    refused(
        &good.replacen("origin_kernel_c c", "origin_kernel_b c", 1),
        4,
    );
    refused(
        &good.replacen("origin_kernel_c c", "origin_kernel_c b", 1),
        4,
    );
}
