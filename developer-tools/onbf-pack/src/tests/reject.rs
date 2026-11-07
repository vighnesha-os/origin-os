//! Negative tests: each hard error of the conversion contract, rejected with its own error.

use super::elf_writer::{Phdr, R, RELATIVE, RW, RawElf, TestElf};
use super::options;
use crate::elf::{ElfError, Permissions, SlotProblem};
use crate::krn::pack;
use crate::relr::RelrError;
use crate::{Error, Toolchain, ToolchainError};

/// `pack` must fail with exactly this ELF error.
#[track_caller]
fn rejects(bytes: &[u8], expected: ElfError) {
    assert_eq!(pack(bytes, &options()), Err(Error::Elf(expected)));
}

fn header(edit: impl FnOnce(&mut RawElf)) -> Vec<u8> {
    let mut raw = TestElf::kernel().raw();
    edit(&mut raw);
    raw.bytes()
}

fn with(edit: impl FnOnce(&mut TestElf)) -> Vec<u8> {
    let mut elf = TestElf::kernel();
    edit(&mut elf);
    elf.bytes()
}

#[test]
fn the_baseline_is_accepted() {
    // Every negative test below changes one thing in this image.
    assert!(pack(&TestElf::kernel().bytes(), &options()).is_ok());
}

// ------------------------------------------------------------- ONBF-R11: 64-bit only

#[test]
fn elfclass32_is_rejected() {
    // ELFCLASS32 for another architecture (EM_ARM).
    rejects(
        &header(|r| {
            r.class = 1;
            r.machine = 40;
        }),
        ElfError::Elf32 { machine: 40 },
    );
}

#[test]
fn em_386_is_rejected() {
    rejects(
        &header(|r| {
            r.class = 1;
            r.machine = 3;
        }),
        ElfError::I386,
    );
    // Also with a 64-bit class.
    rejects(&header(|r| r.machine = 3), ElfError::I386);
}

#[test]
fn x32_is_rejected() {
    // x32 is ELFCLASS32 with EM_X86_64.
    rejects(&header(|r| r.class = 1), ElfError::X32);
}

#[test]
fn other_header_fields_are_checked() {
    rejects(b"\x7FELG\x02\x01\x01", ElfError::NotElf);
    rejects(&header(|r| r.class = 3), ElfError::UnsupportedClass(3));
    rejects(
        &header(|r| r.data = 2),
        ElfError::UnsupportedDataEncoding(2),
    );
    rejects(
        &header(|r| r.ident_version = 0),
        ElfError::UnsupportedVersion(0),
    );
    rejects(&header(|r| r.version = 2), ElfError::UnsupportedVersion(2));
    rejects(&header(|r| r.e_type = 2), ElfError::NotPie(2));
    rejects(
        &header(|r| r.machine = 183),
        ElfError::UnsupportedMachine(183),
    );
    rejects(
        &header(|r| r.phentsize = 64),
        ElfError::HeaderSize {
            ehsize: 64,
            phentsize: 64,
        },
    );
}

// ------------------------------------------------------------- program headers

#[test]
fn pt_interp_is_rejected() {
    rejects(&with(|e| e.extra.push(Phdr::other(3, R))), ElfError::Interp);
}

#[test]
fn pt_tls_is_rejected() {
    rejects(&with(|e| e.extra.push(Phdr::other(7, R))), ElfError::Tls);
}

#[test]
fn pt_gnu_relro_is_rejected() {
    rejects(
        &with(|e| e.extra.push(Phdr::other(0x6474_E552, R))),
        ElfError::GnuRelro,
    );
}

#[test]
fn unknown_program_headers_are_rejected() {
    // PT_NULL and PT_GNU_PROPERTY are outside the contract.
    rejects(
        &with(|e| e.extra.push(Phdr::other(0, 0))),
        ElfError::UnsupportedProgramHeader(0),
    );
    rejects(
        &with(|e| e.extra.push(Phdr::other(0x6474_E553, R))),
        ElfError::UnsupportedProgramHeader(0x6474_E553),
    );
}

#[test]
fn ignored_program_headers_are_accepted() {
    // PT_NOTE, PT_PHDR and PT_GNU_EH_FRAME (coordinator amendment) carry nothing a KRN
    // image needs; the bytes they describe travel inside their PT_LOAD.
    let bytes = with(|e| {
        e.extra.push(Phdr::other(4, R));
        e.extra.push(Phdr::other(6, R));
        e.extra.push(Phdr::other(0x6474_E550, R));
    });
    assert!(pack(&bytes, &options()).is_ok());
}

#[test]
fn duplicate_dynamic_and_stack_headers_are_rejected() {
    rejects(
        &with(|e| e.extra.push(Phdr::other(0x6474_E551, RW))),
        ElfError::DuplicateProgramHeader(0x6474_E551),
    );
    let bytes = with(|e| {
        e.force_dynamic = true;
        e.extra.push(Phdr::other(2, RW));
    });
    rejects(&bytes, ElfError::DuplicateProgramHeader(2));
}

#[test]
fn write_and_execute_segment_is_rejected() {
    rejects(
        &with(|e| e.loads[0].flags = 7),
        ElfError::WriteAndExecute { header: 0 },
    );
    // W+X without R is still W+X.
    rejects(
        &with(|e| e.loads[2].flags = 3),
        ElfError::WriteAndExecute { header: 2 },
    );
}

#[test]
fn other_segment_flags_are_rejected() {
    for flags in [0, 1, 2, 0x4 | 0x0010_0000] {
        rejects(
            &with(|e| e.loads[1].flags = flags),
            ElfError::SegmentFlags { header: 1, flags },
        );
    }
}

#[test]
fn executable_stack_is_rejected() {
    rejects(&with(|e| e.extra[0].flags = 7), ElfError::ExecutableStack);
    rejects(&with(|e| e.extra[0].flags = 1), ElfError::ExecutableStack);
    // A missing PT_GNU_STACK is allowed.
    assert!(pack(&with(|e| e.extra.clear()), &options()).is_ok());
}

#[test]
fn misaligned_pt_load_is_rejected() {
    let misaligned = ElfError::SegmentAlignment { header: 1 };
    rejects(&with(|e| e.loads[1].vaddr = 0x2100), misaligned);
    rejects(&header(|r| r.phdrs[1].offset += 8), misaligned);
    rejects(&header(|r| r.phdrs[1].align = 0x20_0000), misaligned);
    rejects(&header(|r| r.phdrs[1].align = 8), misaligned);
}

#[test]
fn overlapping_or_descending_pt_loads_are_rejected() {
    // Starts inside the previous segment's memory.
    rejects(
        &with(|e| e.loads[1].vaddr = 0x1000),
        ElfError::SegmentOrder { header: 1 },
    );
    // Starts in the previous segment's last, partly used page: no page may hold two
    // permissions (ONBF §6.1). 0x1000 + 0x1001 bytes rounds up to 0x3000.
    rejects(
        &with(|e| e.loads[0].mem_size = 0x1001),
        ElfError::SegmentOrder { header: 1 },
    );
    // Descending order.
    rejects(
        &with(|e| e.loads.swap(0, 1)),
        ElfError::SegmentOrder { header: 1 },
    );
    // Touching page-rounded ranges are fine, and so are gaps.
    let ok = with(|e| {
        e.loads[0].mem_size = 0x1000;
        e.loads[2].vaddr = 0x8000;
    });
    assert!(pack(&ok, &options()).is_ok());
}

#[test]
fn image_span_over_2_gib_is_rejected() {
    rejects(
        &with(|e| e.loads[2].vaddr = 0x8000_0000),
        ElfError::ImageSpan,
    );
    rejects(
        &with(|e| e.loads[2].mem_size = 0x8000_0000),
        ElfError::ImageSpan,
    );
    // Overflow, not just a large value.
    rejects(
        &with(|e| e.loads[2].vaddr = u64::MAX - 0xFFF),
        ElfError::ImageSpan,
    );
    // Ending exactly at 2 GiB is allowed: 0x7FFF_F000 plus one page.
    let ok = with(|e| e.loads[2].vaddr = 0x7FFF_F000);
    assert!(pack(&ok, &options()).is_ok());
}

#[test]
fn other_segment_sizes_are_checked() {
    rejects(
        &with(|e| e.loads[1].mem_size = 4),
        ElfError::FileSizeExceedsMemSize { header: 1 },
    );
    rejects(
        &with(|e| {
            e.loads[1].data.clear();
            e.loads[1].mem_size = 0;
        }),
        ElfError::EmptySegment { header: 1 },
    );
    rejects(&with(|e| e.loads.clear()), ElfError::NoLoadSegments);
}

#[test]
fn entry_not_in_rx_is_rejected() {
    // In the RW segment.
    rejects(
        &with(|e| e.entry = 0x3000),
        ElfError::EntryNotExecutable(0x3000),
    );
    // In the R segment.
    rejects(
        &with(|e| e.entry = 0x2000),
        ElfError::EntryNotExecutable(0x2000),
    );
    // In the zero-filled tail of the RX segment, which has no code.
    rejects(
        &with(|e| {
            e.loads[0].mem_size = 0x800;
            e.entry = 0x1400;
        }),
        ElfError::EntryNotExecutable(0x1400),
    );
    // Outside every segment.
    rejects(
        &with(|e| e.entry = 0x10_0000),
        ElfError::EntryNotExecutable(0x10_0000),
    );
}

// ------------------------------------------------------------- dynamic table

#[test]
fn unknown_dynamic_tag_is_rejected() {
    // DT_TEXTREL, DT_JMPREL, DT_PLTGOT, DT_SONAME, DT_RELCOUNT, DT_VERSYM, DT_REL.
    for tag in [22, 23, 3, 14, 0x6FFF_FFFA, 0x6FFF_FFF0, 17] {
        rejects(
            &with(|e| e.dynamic.push((tag, 0))),
            ElfError::UnknownDynamicTag(tag),
        );
    }
}

#[test]
fn dt_needed_is_rejected() {
    rejects(&with(|e| e.dynamic.push((1, 1))), ElfError::Needed);
}

#[test]
fn static_constructors_are_rejected() {
    // DT_INIT, DT_FINI, DT_INIT_ARRAY, DT_FINI_ARRAY, their sizes, and the preinit pair.
    for tag in [12, 13, 25, 26, 27, 28, 32, 33] {
        rejects(
            &with(|e| e.dynamic.push((tag, 0x2000))),
            ElfError::StaticConstructors(tag),
        );
    }
}

#[test]
fn metadata_tags_are_ignored() {
    // What rust-lld writes for a static PIE: flags, debug, and the symbol and hash tables.
    let bytes = with(|e| {
        e.dynamic = vec![
            (30, 0x8),
            (0x6FFF_FFFB, 0x0800_0001),
            (21, 0),
            (6, 0x2000),
            (11, 24),
            (5, 0x2000),
            (10, 1),
            (0x6FFF_FEF5, 0x2000),
            (4, 0x2000),
        ];
    });
    assert!(pack(&bytes, &options()).is_ok());
}

#[test]
fn malformed_dynamic_tables_are_rejected() {
    let mut elf = TestElf::kernel();
    elf.dynamic.push((21, 0));
    let r = elf.raw();
    let dynamic = r.phdrs.iter().position(|p| p.p_type == 2).unwrap();
    // Not terminated by DT_NULL: PT_DYNAMIC stops before the last entry.
    let mut short = r.clone();
    short.phdrs[dynamic].filesz -= 16;
    rejects(
        &short.bytes(),
        ElfError::DynamicTable("no DT_NULL terminator"),
    );
    // A size that is not a whole number of entries.
    let mut odd = r.clone();
    odd.phdrs[dynamic].filesz -= 8;
    rejects(
        &odd.bytes(),
        ElfError::DynamicTable("PT_DYNAMIC size is not a multiple of 16"),
    );
    // Outside the file.
    let mut outside = r;
    outside.phdrs[dynamic].offset = 0x10_0000;
    rejects(&outside.bytes(), ElfError::Truncated("dynamic table"));
}

#[test]
fn duplicate_relocation_tags_are_rejected() {
    rejects(
        &with(|e| {
            e.rela.push((0x3000, RELATIVE, 0x1000));
            e.dynamic.push((8, 24));
        }),
        ElfError::DuplicateDynamicTag(8),
    );
}

#[test]
fn relocation_table_shape_is_checked() {
    let rela = |e: &mut TestElf| e.rela.push((0x3000, RELATIVE, 0x1000));
    rejects(
        &with(|e| {
            rela(e);
            e.relaent = 16;
        }),
        ElfError::RelaEntrySize(16),
    );
    rejects(
        &with(|e| {
            e.relr.push(0x3000);
            e.relrent = 16;
        }),
        ElfError::RelrEntrySize(16),
    );
    rejects(
        &with(|e| e.dynamic.push((7, 0x4000))),
        ElfError::RelocationTags("DT_RELA, DT_RELASZ and DT_RELAENT must appear together"),
    );
    rejects(
        &with(|e| e.dynamic.push((36, 0x4000))),
        ElfError::RelocationTags("DT_RELR, DT_RELRSZ and DT_RELRENT must appear together"),
    );
    rejects(
        &with(|e| e.dynamic.push((0x6FFF_FFF9, 1))),
        ElfError::RelocationTags("DT_RELACOUNT without DT_RELA"),
    );
    // DT_RELASZ that is not a multiple of 24, built by hand.
    rejects(
        &with(|e| e.dynamic = vec![(7, 0x2000), (8, 20), (9, 24)]),
        ElfError::RelocationTable("DT_RELASZ is not a multiple of 24"),
    );
    rejects(
        &with(|e| e.dynamic = vec![(36, 0x2000), (35, 12), (37, 8)]),
        ElfError::RelocationTable("DT_RELRSZ is not a multiple of 8"),
    );
    // A table that is not in the file-backed bytes of one segment.
    rejects(
        &with(|e| e.dynamic = vec![(7, 0x3040), (8, 48), (9, 24)]),
        ElfError::RelocationTable("DT_RELA is not inside the file-backed bytes of one PT_LOAD"),
    );
    rejects(
        &with(|e| e.dynamic = vec![(36, 0x9000), (35, 8), (37, 8)]),
        ElfError::RelocationTable("DT_RELR is not inside the file-backed bytes of one PT_LOAD"),
    );
    // A second DT_RELACOUNT.
    rejects(
        &with(|e| {
            rela(e);
            e.dynamic.push((0x6FFF_FFF9, 5));
        }),
        ElfError::DuplicateDynamicTag(0x6FFF_FFF9),
    );
}

#[test]
fn relacount_above_entries_is_rejected() {
    // The writer emits DT_RELACOUNT = entries; patch it to 2 with one entry.
    let mut elf = TestElf::kernel();
    elf.rela.push((0x3000, RELATIVE, 0x1000));
    let mut raw = elf.raw();
    let meta = raw
        .phdrs
        .iter()
        .find(|p| p.p_type == 1 && p.vaddr == 0x4000)
        .unwrap()
        .offset;
    // Table: 24 bytes of RELA, then the dynamic entries RELA, RELASZ, RELAENT, RELACOUNT.
    let relacount_value = meta + 24 + 3 * 16 + 8;
    raw.put(relacount_value, &2u64.to_le_bytes());
    rejects(
        &raw.bytes(),
        ElfError::RelaCount {
            count: 2,
            entries: 1,
        },
    );
}

// ------------------------------------------------------------- relocations

#[test]
fn non_relative_relocation_is_rejected() {
    // R_X86_64_64 against symbol 1, R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT,
    // R_X86_64_IRELATIVE, R_X86_64_DTPMOD64 and R_X86_64_NONE.
    for r_type in [1u32, 6, 7, 37, 16, 0] {
        let info = (1u64 << 32) | u64::from(r_type);
        rejects(
            &with(|e| e.rela.push((0x3000, info, 0))),
            ElfError::UnsupportedRelocation {
                slot: 0x3000,
                r_type,
            },
        );
    }
    // RELATIVE must not name a symbol.
    rejects(
        &with(|e| e.rela.push((0x3000, (2 << 32) | RELATIVE, 0))),
        ElfError::RelocationSymbol {
            slot: 0x3000,
            symbol: 2,
        },
    );
}

#[test]
fn relocation_slot_in_bss_is_rejected() {
    // The RW segment has 64 file-backed bytes, then bss up to 0x100.
    for slot in [0x3040, 0x3080, 0x30F8] {
        rejects(
            &with(|e| e.rela.push((slot, RELATIVE, 0x1000))),
            ElfError::RelocationSlot {
                slot,
                problem: SlotProblem::Bss,
            },
        );
    }
    // The same through DT_RELR.
    rejects(
        &with(|e| e.relr.push(0x3040)),
        ElfError::RelocationSlot {
            slot: 0x3040,
            problem: SlotProblem::Bss,
        },
    );
}

#[test]
fn relocation_slot_in_rx_segment_is_rejected() {
    rejects(
        &with(|e| e.rela.push((0x1008, RELATIVE, 0x1000))),
        ElfError::RelocationSlot {
            slot: 0x1008,
            problem: SlotProblem::NotWritable(Permissions::ReadExecute),
        },
    );
    rejects(
        &with(|e| e.relr.push(0x1008)),
        ElfError::RelocationSlot {
            slot: 0x1008,
            problem: SlotProblem::NotWritable(Permissions::ReadExecute),
        },
    );
}

#[test]
fn relocation_slot_in_r_segment_or_unmapped_is_rejected() {
    rejects(
        &with(|e| e.rela.push((0x2008, RELATIVE, 0))),
        ElfError::RelocationSlot {
            slot: 0x2008,
            problem: SlotProblem::NotWritable(Permissions::Read),
        },
    );
    rejects(
        &with(|e| e.rela.push((0x9000, RELATIVE, 0))),
        ElfError::RelocationSlot {
            slot: 0x9000,
            problem: SlotProblem::Unmapped,
        },
    );
}

#[test]
fn misaligned_relocation_slot_is_rejected() {
    for slot in [0x3004, 0x3001, 0x3006] {
        rejects(
            &with(|e| e.rela.push((slot, RELATIVE, 0x1000))),
            ElfError::RelocationSlot {
                slot,
                problem: SlotProblem::Misaligned,
            },
        );
    }
    // A RELR address word with bit 0 clear but bit 2 set.
    rejects(
        &with(|e| e.relr.push(0x3004)),
        ElfError::RelocationSlot {
            slot: 0x3004,
            problem: SlotProblem::Misaligned,
        },
    );
}

#[test]
fn conflicting_relocations_are_rejected() {
    rejects(
        &with(|e| {
            e.rela.push((0x3008, RELATIVE, 0x1000));
            e.rela.push((0x3008, RELATIVE, 0x2000));
        }),
        ElfError::RelocationConflict { slot: 0x3008 },
    );
    rejects(
        &with(|e| {
            e.rela.push((0x3008, RELATIVE, 0x1000));
            e.relr.push(0x3008);
        }),
        ElfError::RelocationConflict { slot: 0x3008 },
    );
}

#[test]
fn bad_relr_tables_are_rejected() {
    rejects(
        &with(|e| e.relr.push(0b11)),
        ElfError::Relr(RelrError::BitmapWithoutAddress { word: 0 }),
    );
    rejects(
        &with(|e| e.relr = vec![0x3010, 0x3008]),
        ElfError::Relr(RelrError::NotAscending { slot: 0x3008 }),
    );
}

#[test]
fn truncated_file_is_rejected() {
    let mut elf = TestElf::kernel();
    elf.rela.push((0x3008, RELATIVE, 0x2000));
    let bytes = elf.bytes();
    assert!(pack(&bytes, &options()).is_ok());
    // Every strict prefix fails, and always as a truncation: the last byte of the file
    // belongs to the last PT_LOAD.
    for len in 0..bytes.len() {
        match pack(&bytes[..len], &options()) {
            Err(Error::Elf(ElfError::Truncated(_))) => {}
            other => panic!("prefix of {len} bytes: {other:?}"),
        }
    }
    // Specific places.
    assert_eq!(
        pack(&bytes[..40], &options()),
        Err(Error::Elf(ElfError::Truncated("ELF header")))
    );
    assert_eq!(
        pack(&bytes[..100], &options()),
        Err(Error::Elf(ElfError::Truncated("program header table")))
    );
    assert_eq!(
        pack(&bytes[..4096 + 10], &options()),
        Err(Error::Elf(ElfError::Truncated("file range of a PT_LOAD")))
    );
}

// ------------------------------------------------------------- options

#[test]
fn reserved_cpu_feature_bits_are_rejected() {
    let bytes = TestElf::kernel().bytes();
    let mut o = options();
    o.cpu_features = 0x40 | 0x1;
    assert_eq!(pack(&bytes, &o), Err(Error::CpuFeatures(0x40)));
    o.cpu_features = 1 << 63;
    assert_eq!(pack(&bytes, &o), Err(Error::CpuFeatures(1 << 63)));
    o.cpu_features = 0x3F;
    assert!(pack(&bytes, &o).is_ok());
}

#[test]
fn invalid_toolchain_fields_are_rejected() {
    type Edit = fn(&mut Toolchain);
    let bytes = TestElf::kernel().bytes();
    let cases: [(Edit, &str); 4] = [
        (|t| t.rustc_commit.make_ascii_uppercase(), "rustc_commit"),
        (|t| t.rustc_commit.truncate(39), "rustc_commit"),
        (|t| t.rustc = "1.99.1\n".into(), "rustc"),
        (|t| t.llvm.clear(), "llvm"),
    ];
    for (edit, field) in cases {
        let mut o = options();
        edit(&mut o.toolchain);
        match pack(&bytes, &o) {
            Err(Error::Toolchain(ToolchainError::Invalid { field: f, .. })) => {
                assert_eq!(f, field);
            }
            other => panic!("{field}: {other:?}"),
        }
    }
}
