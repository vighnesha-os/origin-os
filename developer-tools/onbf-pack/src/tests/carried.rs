//! `carried`: a program laid out for the kernel (task S4-1). Each of ONBF §6.1.2's hard errors is
//! refused, one fixture each; the addends land in the bytes; the same input gives the same
//! files.

use super::elf_writer::{Load, Phdr, R, RELATIVE, RW, RX, TestElf, text};
use crate::carried::{self, Access, CarriedError, File, describe};

/// [`carried::stale`] for the program `first`, whose descriptions begin with the line
/// `onbf-pack carried` writes for it.
fn stale(
    name: &str,
    files: &[File],
    present: &[String],
    description: Option<&[u8]>,
) -> Result<Vec<String>, CarriedError> {
    carried::stale(
        name,
        &carried::generated_line(name),
        files,
        present,
        description,
    )
}
use crate::elf::{ElfError, Permissions, SlotProblem};

/// The baseline program: text at 0x1000 (the entry), read-only data at 0x2000, writable data at
/// 0x3000 with bss, and two relative relocations into the writable data.
fn program() -> TestElf {
    let mut elf = TestElf::kernel();
    elf.rela = vec![(0x3000, RELATIVE, 0x2008), (0x3010, RELATIVE, 0x1004)];
    elf
}

#[track_caller]
fn refuses(elf: &TestElf, expected: ElfError) {
    assert_eq!(describe(&elf.bytes()), Err(CarriedError::Elf(expected)));
}

#[test]
fn the_baseline_is_laid_out_with_its_addends_in_place() {
    let carried = describe(&program().bytes()).expect("the baseline converts");
    assert_eq!(carried.entry, 0x1000);
    let shape: Vec<(u64, u64, Access)> = carried
        .segments
        .iter()
        .map(|s| (s.rva, s.size, s.access))
        .collect();
    // The table segment the writer adds after the loads is read-only data too.
    assert_eq!(
        shape[..3],
        [
            (0x1000, 0x1000, Access::ReadExecute),
            (0x2000, 0x1000, Access::Read),
            (0x3000, 0x1000, Access::ReadWrite),
        ]
    );
    assert_eq!(carried.relocations, [0x3000, 0x3010]);
    let data = &carried.segments[2].bytes;
    assert_eq!(data.len(), 64, "only the file-backed bytes are carried");
    assert_eq!(data[0..8], 0x2008u64.to_le_bytes());
    assert_eq!(data[16..24], 0x1004u64.to_le_bytes());
    assert!(data[8..16].iter().all(|&b| b == 0) && data[24..].iter().all(|&b| b == 0));
    assert_eq!(carried.segments[0].bytes, text(64));
    let last = carried.segments.last().unwrap();
    assert_eq!(carried.span, last.rva + last.size);
}

#[test]
fn a_relro_segment_is_read_only_after_relocation() {
    let mut elf = program();
    elf.loads[2] = Load::new(RW, 0x3000, vec![0; 64]).mem(0x2000);
    elf.extra.push(Phdr::relro(0x3000, 0x1000));
    let carried = describe(&elf.bytes()).unwrap();
    let access: Vec<Access> = carried.segments.iter().map(|s| s.access).collect();
    assert_eq!(
        access[..4],
        [
            Access::ReadExecute,
            Access::Read,
            Access::ReadAfterRelocation,
            Access::ReadWrite
        ]
    );
}

// ------------------------------------------------- ONBF §6.1.2's hard errors, one each

#[test]
fn a_writable_and_executable_segment_is_refused() {
    let mut elf = program();
    elf.loads[0].flags = RX | RW;
    refuses(&elf, ElfError::WriteAndExecute { header: 0 });
}

#[test]
fn a_text_relocation_is_refused() {
    let mut elf = program();
    elf.rela.push((0x1008, RELATIVE, 0));
    refuses(
        &elf,
        ElfError::RelocationSlot {
            slot: 0x1008,
            problem: SlotProblem::NotWritable(Permissions::ReadExecute),
        },
    );
    let mut elf = program();
    elf.dynamic.push((22, 0));
    refuses(&elf, ElfError::UnknownDynamicTag(22));
}

#[test]
fn pt_interp_is_refused() {
    let mut elf = program();
    elf.extra.push(Phdr::other(3, R));
    refuses(&elf, ElfError::Interp);
}

#[test]
fn an_executable_stack_is_refused() {
    let mut elf = program();
    elf.extra = vec![Phdr::other(0x6474_E551, RW | RX)];
    refuses(&elf, ElfError::ExecutableStack);
}

#[test]
fn static_constructors_are_refused() {
    // DT_INIT_ARRAY and its size, DT_PREINIT_ARRAY, DT_FINI_ARRAY.
    for tag in [25, 27, 32, 26] {
        let mut elf = program();
        elf.dynamic.push((tag, 0x3020));
        refuses(&elf, ElfError::StaticConstructors(tag));
    }
}

#[test]
fn an_undefined_symbol_is_refused() {
    let mut elf = program();
    // DT_HASH (one bucket, two symbols) at 0x5000, then the null symbol and one undefined.
    let mut table = Vec::new();
    for word in [1u32, 2, 0, 0] {
        table.extend_from_slice(&word.to_le_bytes());
    }
    table.extend_from_slice(&[0; 48]);
    elf.loads.push(Load::new(R, 0x5000, table));
    elf.dynamic.extend([(6, 0x5010), (11, 24), (4, 0x5000)]);
    refuses(&elf, ElfError::UndefinedSymbol { index: 1 });
}

#[test]
fn every_relocation_but_relative_is_refused() {
    // R_X86_64_64, R_X86_64_GLOB_DAT and R_X86_64_JUMP_SLOT (nothing to bind until S4-2), and
    // R_X86_64_TPOFF64 (no thread-local storage).
    for r_type in [1u32, 6, 7, 18] {
        let mut elf = program();
        elf.rela.push((0x3020, (1 << 32) | u64::from(r_type), 0));
        refuses(
            &elf,
            ElfError::UnsupportedRelocation {
                slot: 0x3020,
                r_type,
            },
        );
    }
}

#[test]
fn an_image_span_over_2_gib_is_refused() {
    let mut elf = program();
    elf.loads[2].mem_size = 0x8000_0000;
    refuses(&elf, ElfError::ImageSpan);
}

#[test]
fn a_dt_needed_is_refused() {
    let mut elf = program();
    elf.dynamic.push((1, 1));
    refuses(&elf, ElfError::Needed);
}

// ------------------------------------------------------------------ the program's own rule

#[test]
fn exactly_one_read_execute_segment() {
    // A second read-execute segment after the writable data. (With none, the entry is not in
    // one, which the reader refuses first.)
    let mut two = program();
    two.loads.push(Load::new(RX, 0x4000, text(16)));
    assert_eq!(describe(&two.bytes()), Err(CarriedError::CodeSegments(2)));
}

// ------------------------------------------------------------------ the generated files

#[test]
fn the_same_input_gives_the_same_files() {
    let bytes = program().bytes();
    let one = describe(&bytes).unwrap().rust("first", "PROGRAMS").unwrap();
    let two = describe(&bytes).unwrap().rust("first", "PROGRAMS").unwrap();
    assert_eq!(one, two);
    // One Rust file and one file of bytes for each segment that has any.
    let names: Vec<&str> = one.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names[..4],
        ["first.rs", "first.0.bin", "first.1.bin", "first.2.bin"]
    );
    // A different input gives a different description.
    let mut other = program();
    other.rela[0].2 = 0x2010;
    let three = describe(&other.bytes())
        .unwrap()
        .rust("first", "PROGRAMS")
        .unwrap();
    assert_ne!(one, three);
}

#[test]
fn the_rust_file_says_what_the_kernel_maps() {
    let files = describe(&program().bytes())
        .unwrap()
        .rust("first", "PROGRAMS")
        .unwrap();
    let text = String::from_utf8(files[0].1.clone()).unwrap();
    for line in [
        "pub(crate) static FIRST: Carried = Carried {",
        "    name: \"first\",",
        "    entry: 0x1000,",
        "            access: SegmentAccess::ReadExecute,",
        "            bytes: include_bytes!(concat!(env!(\"PROGRAMS\"), \"/first.0.bin\")),",
        "        0x3000, 0x3010,",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }
    assert!(!text.contains('/') || text.matches("include_bytes!").count() == files.len() - 1);
}

#[test]
fn names_are_checked() {
    let carried = describe(&program().bytes()).unwrap();
    for bad in ["", "First", "1st", "first-program", "a/b", &"x".repeat(65)] {
        assert_eq!(
            carried.rust(bad, "PROGRAMS"),
            Err(CarriedError::Name(bad.to_string()))
        );
    }
    for bad in ["", "programs", "9P", "A-B"] {
        assert_eq!(
            carried.rust("first", bad),
            Err(CarriedError::Variable(bad.to_string()))
        );
    }
    assert!(carried.rust("first_program_2", "ORIGIN_CARRIED_2").is_ok());
}

/// What `stale` is given for a directory: its file names, and its description's bytes.
fn directory(files: &[&str]) -> Vec<String> {
    files.iter().map(|f| (*f).to_string()).collect()
}

#[test]
fn only_this_tools_own_stale_files_are_removed() {
    let carried = describe(&program().bytes()).unwrap();
    let files = carried.rust("first", "ORIGIN_CARRIED").unwrap();
    let ours = files[0].1.clone();
    assert_eq!(files[0].0, "first.rs");
    // An empty directory, and one with nothing of this program's: nothing to remove.
    assert_eq!(stale("first", &files, &[], None), Ok(vec![]));
    let others = directory(&["second.rs", "second.0.bin", "notes.txt"]);
    assert_eq!(stale("first", &files, &others, None), Ok(vec![]));
    // Its own earlier description, with more segments: the extra files go, nothing else does,
    // whatever its name looks like.
    let present = directory(&[
        "first.rs",
        "first.0.bin",
        "first.1.bin",
        "first.7.bin",
        "first.12.bin",
        "first.txt",
        "first.rs.bak",
        "first.x.bin",
        "first..bin",
        "first.0.bin.old",
        "firsts.9.bin",
        "second.3.bin",
    ]);
    let mut gone = stale("first", &files, &present, Some(&ours)).unwrap();
    gone.sort();
    let kept: Vec<&String> = files.iter().map(|(name, _)| name).collect();
    assert!(gone.iter().all(|f| !kept.contains(&f)));
    assert!(
        gone.contains(&"first.7.bin".to_string()) && gone.contains(&"first.12.bin".to_string())
    );
    assert!(
        gone.iter().all(|f| f.starts_with("first.")
            && f.ends_with(".bin")
            && f != "first.x.bin"
            && f != "first..bin"),
        "{gone:?}"
    );
}

#[test]
fn a_directory_this_tool_did_not_write_to_is_refused() {
    let carried = describe(&program().bytes()).unwrap();
    let files = carried.rust("first", "ORIGIN_CARRIED").unwrap();
    let ours = files[0].1.clone();
    // A hand-written `first.rs`: the wrong `--out`.
    assert_eq!(
        stale(
            "first",
            &files,
            &directory(&["first.rs", "lib.rs"]),
            Some(b"fn main() {}\n")
        ),
        Err(CarriedError::Foreign("first.rs".into()))
    );
    // Bytes files and no description, or one that cannot be read.
    assert_eq!(
        stale("first", &files, &directory(&["first.0.bin"]), None),
        Err(CarriedError::Foreign("first.0.bin".into()))
    );
    assert_eq!(
        stale("first", &files, &directory(&["first.rs"]), None),
        Err(CarriedError::Foreign("first.rs".into()))
    );
    // Another program's description under this one's name.
    let other = carried.rust("second", "ORIGIN_CARRIED").unwrap();
    assert_eq!(
        stale(
            "first",
            &files,
            &directory(&["first.rs"]),
            Some(&other[0].1)
        ),
        Err(CarriedError::Foreign("first.rs".into()))
    );
    // Its own: accepted.
    assert_eq!(
        stale("first", &files, &directory(&["first.rs"]), Some(&ours)),
        Ok(vec![])
    );
}
