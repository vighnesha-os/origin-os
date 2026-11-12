//! The launch path's arithmetic on hand-made descriptions: [`check`]'s rules one by one, the
//! pages [`fill_page`] builds, and where [`random_base`] puts a program.

use super::*;
use crate::core_image::{CoreExport, PAGE as CORE_PAGE};
use origin_syscalls::origin_kernel::kernel::server::{Pins, RELEASE_FINGERPRINT};

/// An `origin.core` of three exports, for the binding's arithmetic alone: `check` and
/// `fill_page` read its exports and its fingerprint, and nothing else of it.
static CORE_EXPORTS: [CoreExport; 3] = [
    CoreExport {
        name: "origin_kernel_a",
        offset: 0x10,
        since: (0, 0),
    },
    CoreExport {
        name: "origin_kernel_b",
        offset: 0x1C,
        since: (0, 0),
    },
    CoreExport {
        name: "origin_kernel_c",
        offset: 0,
        since: (0, 5),
    },
];
static CORE: CoreImage = CoreImage {
    span: CORE_PAGE,
    code: &[0xCC; 64],
    exports: &CORE_EXPORTS,
    pins: Pins {
        thread_exit: 0,
        debug_report: 0,
        debug_write: 0,
        handle_close: 0,
        handle_duplicate: 0,
        channel_create: 0,
        channel_write: 0,
        channel_read: 0,
        wait_one: 0,
        event_create: 0,
        object_signal: 0,
        memory_object_create: 0,
        memory_map: 0,
        memory_unmap: 0,
        memory_protect: 0,
        port_create: 0,
        object_wait_async: 0,
        port_wait: 0,
        port_cancel: 0,
        clock_read: 0,
    },
    release: RELEASE_FINGERPRINT,
};
/// Where the tests put `origin.core` in a process.
const CORE_BASE: u64 = 0x0000_0456_7000_0000;
const AT: CoreAt<'static> = CoreAt {
    base: CORE_BASE,
    image: &CORE,
};

/// Text at 0x1000 (the entry at 0x1010), read-only data at 0x2000, a RELRO page at 0x3000 with
/// two slots, and writable data at 0x4000 with one slot and two pages, the second all bss.
static TEXT: [u8; 32] = [0xCC; 32];
static RODATA: [u8; 8] = *b"readonly";
static RELRO: [u8; 24] = {
    let mut b = [0u8; 24];
    let (a, c) = (0x2000u64.to_le_bytes(), 0x1010u64.to_le_bytes());
    let mut i = 0;
    while i < 8 {
        b[i] = a[i];
        b[16 + i] = c[i];
        i += 1;
    }
    b
};
static DATA: [u8; 4096] = {
    let mut b = [0x11u8; 4096];
    let a = 0x3008u64.to_le_bytes();
    let mut i = 0;
    while i < 8 {
        b[4088 + i] = a[i];
        i += 1;
    }
    b
};
static SEGMENTS: [Segment; 4] = [
    Segment {
        rva: 0x1000,
        size: 0x1000,
        access: SegmentAccess::ReadExecute,
        bytes: &TEXT,
    },
    Segment {
        rva: 0x2000,
        size: 0x1000,
        access: SegmentAccess::Read,
        bytes: &RODATA,
    },
    Segment {
        rva: 0x3000,
        size: 0x1000,
        access: SegmentAccess::ReadAfterRelocation,
        bytes: &RELRO,
    },
    Segment {
        rva: 0x4000,
        size: 0x2000,
        access: SegmentAccess::ReadWrite,
        bytes: &DATA,
    },
];
static SLOTS: [u64; 3] = [0x3000, 0x3010, 0x4FF8];

const GOOD: Carried = Carried {
    name: "test",
    entry: 0x1010,
    span: 0x6000,
    segments: &SEGMENTS,
    relocations: &SLOTS,
    imports: &[],
    core: None,
};

/// `GOOD`, bound against [`CORE`]: its RELRO page's second word, between the two relocation
/// slots, takes export `b`.
static IMPORTS: [Import; 1] = [Import {
    slot: 0x3008,
    export: 1,
}];
const BOUND: Carried = Carried {
    imports: &IMPORTS,
    core: Some(RELEASE_FINGERPRINT),
    ..GOOD
};

/// `GOOD` with `segments` in place of its own.
fn with_segments(segments: &'static [Segment]) -> Carried {
    Carried { segments, ..GOOD }
}

/// A leaked copy of `SEGMENTS` after `edit`.
fn segments(edit: impl FnOnce(&mut Vec<Segment>)) -> &'static [Segment] {
    let mut s = SEGMENTS.to_vec();
    edit(&mut s);
    Vec::leak(s)
}

#[test]
fn a_good_description_passes() {
    assert_eq!(check(&GOOD, &CORE), Ok(()));
    assert_eq!(
        code_range(&GOOD, 0x10_0000_0000),
        Some((0x10_0000_1000, 0x10_0000_2000))
    );
}

#[test]
fn each_segment_rule_is_held() {
    static LONG: [u8; 4097] = [0; 4097];
    let cases: [fn(&mut Vec<Segment>); 6] = [
        |s| s[1].rva = 0x2008,  // not page-aligned
        |s| s[1].size = 0x1800, // not whole pages
        |s| s[1].size = 0,      // maps nothing
        |s| s[2].rva = 0x2000,  // overlaps the one before
        |s| s[1].bytes = &LONG, // more bytes than it maps
        |s| s[3].size = 0x3000, // ends past the span
    ];
    for (i, case) in cases.iter().enumerate() {
        let carried = with_segments(segments(case));
        assert_eq!(
            check(&carried, &CORE),
            Err(LayoutError::Segment),
            "case {i}"
        );
    }
}

#[test]
fn exactly_one_code_segment_holds_the_entry() {
    let two = with_segments(segments(|s| s[1].access = SegmentAccess::ReadExecute));
    assert_eq!(check(&two, &CORE), Err(LayoutError::Code));
    let none = with_segments(segments(|s| s[0].access = SegmentAccess::Read));
    assert_eq!(check(&none, &CORE), Err(LayoutError::Code));
    for entry in [0x0FFF, 0x2000, 0x2010] {
        assert_eq!(
            check(&Carried { entry, ..GOOD }, &CORE),
            Err(LayoutError::Code)
        );
    }
}

#[test]
fn the_span_is_whole_pages_up_to_two_gib() {
    for span in [0, 0x6001, MAX_SPAN + PAGE] {
        assert_eq!(
            check(&Carried { span, ..GOOD }, &CORE),
            Err(LayoutError::Span),
            "{span:#x}"
        );
    }
}

#[test]
fn each_relocation_rule_is_held() {
    for slots in [
        &[0x3010, 0x3000][..], // out of order
        &[0x3000, 0x3000],     // twice
        &[0x3004],             // misaligned
        &[0x1000],             // in the code: a text relocation
        &[0x2000],             // in read-only data
        &[0x3018 + 8],         // past the RELRO page's bytes
        &[0x5000],             // in the bss
        &[0x7000],             // outside every segment
    ] {
        let carried = Carried {
            relocations: Vec::leak(slots.to_vec()),
            ..GOOD
        };
        assert_eq!(
            check(&carried, &CORE),
            Err(LayoutError::Relocation),
            "{slots:x?}"
        );
    }
}

#[test]
fn a_page_holds_its_bytes_with_the_base_added_to_its_slots() {
    let base = 0x0000_0123_4000_0000;
    let mut page = [0xFFu8; PAGE as usize];
    fill_page(&GOOD, 2, 0, base, AT, &mut page).unwrap();
    assert_eq!(page[0..8], (0x2000 + base).to_le_bytes());
    assert_eq!(page[8..16], [0; 8]);
    assert_eq!(page[16..24], (0x1010 + base).to_le_bytes());
    assert!(page[24..].iter().all(|&b| b == 0), "zero after the bytes");

    // The writable data's first page: its bytes, and the slot at its last word.
    fill_page(&GOOD, 3, 0, base, AT, &mut page).unwrap();
    assert!(page[..4088].iter().all(|&b| b == 0x11));
    assert_eq!(page[4088..], (0x3008 + base).to_le_bytes());
    // Its second page is all bss, and no slot of the page before leaks into it.
    fill_page(&GOOD, 3, 1, base, AT, &mut page).unwrap();
    assert!(page.iter().all(|&b| b == 0));

    // Text is copied as it is: no slot is in it.
    fill_page(&GOOD, 0, 0, base, AT, &mut page).unwrap();
    assert_eq!(page[..32], TEXT);
    assert!(page[32..].iter().all(|&b| b == 0));

    // What each slot then holds is what `relocated` says.
    assert_eq!(relocated(&GOOD, 0x3010, base), Some(0x1010 + base));
    assert_eq!(relocated(&GOOD, 0x4FF8, base), Some(0x3008 + base));
}

#[test]
fn a_page_outside_its_segment_or_a_wrong_buffer_is_refused() {
    let mut page = [0u8; PAGE as usize];
    assert_eq!(
        fill_page(&GOOD, 4, 0, 0, AT, &mut page),
        Err(LayoutError::Segment)
    );
    assert_eq!(
        fill_page(&GOOD, 0, 1, 0, AT, &mut page),
        Err(LayoutError::Segment)
    );
    assert_eq!(
        fill_page(&GOOD, 0, 0, 0, AT, &mut page[..100]),
        Err(LayoutError::Segment)
    );
}

#[test]
fn a_base_that_overflows_a_slot_is_refused() {
    let mut page = [0u8; PAGE as usize];
    assert_eq!(
        fill_page(&GOOD, 2, 0, u64::MAX - 0x1000, AT, &mut page),
        Err(LayoutError::Relocation)
    );
}

#[test]
fn every_base_is_a_page_in_the_region_with_room_for_the_program() {
    let pages = GOOD.span / PAGE;
    let slots = (PROGRAM_HIGH - PROGRAM_LOW - GOOD.span) / PAGE + 1;
    assert_eq!(random_base(pages, 0), Some(PROGRAM_LOW));
    assert_eq!(random_base(pages, slots), Some(PROGRAM_LOW));
    let last = random_base(pages, slots - 1).unwrap();
    assert_eq!(last + GOOD.span, PROGRAM_HIGH);
    for random in [1, 0xDEAD_BEEF, u64::MAX, 0x1234_5678_9ABC_DEF0] {
        let base = random_base(pages, random).unwrap();
        assert_eq!(base % PAGE, 0);
        assert!(base >= PROGRAM_LOW && base + GOOD.span <= PROGRAM_HIGH);
    }
    assert_ne!(random_base(pages, 1), random_base(pages, 2));
    // Almost 32 bits of choice: 2^32 - 2^16 pages, less the program's own.
    assert_eq!(slots, (1 << 32) - (1 << 16) - pages + 1);
    assert_eq!(
        random_base((PROGRAM_HIGH - PROGRAM_LOW) / PAGE + 1, 0),
        None
    );
    assert_eq!(random_base(u64::MAX, 0), None);
}

#[test]
fn each_page_of_the_span_has_its_segments_access_or_none() {
    // `GOOD` has no segment at its first page: that page stays unmapped.
    assert_eq!(page_access(&GOOD, 0), None);
    assert_eq!(page_access(&GOOD, 0xFFF), None);
    assert_eq!(page_access(&GOOD, 0x1000), Some(SegmentAccess::ReadExecute));
    assert_eq!(page_access(&GOOD, 0x1FFF), Some(SegmentAccess::ReadExecute));
    assert_eq!(page_access(&GOOD, 0x2000), Some(SegmentAccess::Read));
    assert_eq!(
        page_access(&GOOD, 0x3000),
        Some(SegmentAccess::ReadAfterRelocation)
    );
    // Both pages of the writable segment, the second all bss.
    assert_eq!(page_access(&GOOD, 0x4000), Some(SegmentAccess::ReadWrite));
    assert_eq!(page_access(&GOOD, 0x5000), Some(SegmentAccess::ReadWrite));
    // Past the span.
    assert_eq!(page_access(&GOOD, GOOD.span), None);
    assert_eq!(page_access(&GOOD, u64::MAX), None);
}

#[test]
fn the_largest_stack_and_its_two_neighbours_fit_the_region() {
    assert_eq!(MAX_STACK_PAGES * PAGE, MAX_SPAN);
    let place = random_base(MAX_STACK_PAGES + 2, u64::MAX).unwrap();
    assert!(place + (MAX_STACK_PAGES + 2) * PAGE <= PROGRAM_HIGH);
}

// ------------------------------------------------------------------ imports (ADR-0011 L3)

#[test]
fn a_bound_description_passes_and_its_slot_takes_its_exports_address() {
    assert_eq!(check(&BOUND, &CORE), Ok(()));
    let base = 0x0000_0123_4000_0000;
    let mut page = [0xFFu8; PAGE as usize];
    fill_page(&BOUND, 2, 0, base, AT, &mut page).unwrap();
    // The relocation slots on each side of it are as they were, and the import's slot holds
    // export `b`'s address in this process's `origin.core`: nothing of the program's base.
    assert_eq!(page[0..8], (0x2000 + base).to_le_bytes());
    assert_eq!(page[8..16], (CORE_BASE + 0x1C).to_le_bytes());
    assert_eq!(page[16..24], (0x1010 + base).to_le_bytes());
    assert!(page[24..].iter().all(|&b| b == 0));
    assert_eq!(bound(&IMPORTS[0], AT), Some(CORE_BASE + 0x1C));

    // Another process, another base of `origin.core`: the same slot, that process's address.
    let elsewhere = CoreAt {
        base: 0x0000_0777_0000_0000,
        image: &CORE,
    };
    fill_page(&BOUND, 2, 0, base, elsewhere, &mut page).unwrap();
    assert_eq!(page[8..16], (0x0000_0777_0000_001Cu64).to_le_bytes());

    // No other page takes it: the writable data's pages are as they are without imports.
    let (mut with, mut without) = ([0u8; PAGE as usize], [0u8; PAGE as usize]);
    for (segment, page) in [(0, 0), (1, 0), (3, 0), (3, 1)] {
        fill_page(&BOUND, segment, page, base, AT, &mut with).unwrap();
        fill_page(&GOOD, segment, page, base, AT, &mut without).unwrap();
        assert_eq!(with, without, "segment {segment}, page {page}");
    }
}

#[test]
fn each_import_rule_is_held() {
    let import = |slot, export| Import { slot, export };
    // With no relocation in the way, every slot of the RELRO page's 24 bytes is free.
    for imports in [
        &[import(0x3010, 0), import(0x3008, 1)][..], // out of order
        &[import(0x3008, 0), import(0x3008, 1)],     // one slot twice
        &[import(0x3004, 0)],                        // misaligned
        &[import(0x1000, 0)],                        // in the code
        &[import(0x2000, 0)],                        // in read-only data: nothing writes there
        &[import(0x4000, 0)], // in writable data: the program could change where it calls
        &[import(0x3018, 0)], // past the RELRO page's bytes
        &[import(0x7000, 0)], // outside every segment
        &[import(0x3008, 3)], // an export `origin.core` does not have
        &[import(0x3008, usize::MAX)],
    ] {
        let carried = Carried {
            relocations: &[],
            imports: Vec::leak(imports.to_vec()),
            ..BOUND
        };
        assert_eq!(
            check(&carried, &CORE),
            Err(LayoutError::Import),
            "{imports:x?}"
        );
    }
    // A slot that is also a relocation slot: two writers of one word.
    let both = Carried {
        imports: Vec::leak(vec![import(0x3010, 0)]),
        ..BOUND
    };
    assert_eq!(check(&both, &CORE), Err(LayoutError::Import));
    // The same slots with no relocation there pass: the rule above is what refused it.
    let free = Carried {
        relocations: &[],
        imports: Vec::leak(vec![
            import(0x3000, 2),
            import(0x3008, 1),
            import(0x3010, 0),
        ]),
        ..BOUND
    };
    assert_eq!(check(&free, &CORE), Ok(()));
}

#[test]
fn a_program_is_launched_only_beside_the_origin_core_it_was_bound_against() {
    // Imports with no record of what they were bound against.
    let unbound = Carried {
        core: None,
        ..BOUND
    };
    assert_eq!(check(&unbound, &CORE), Err(LayoutError::Import));
    // Bound against another release's `origin.core`: an export's place means nothing here,
    // whether or not it has an import.
    let mut other = RELEASE_FINGERPRINT;
    other[31] ^= 1;
    let stale = Carried {
        core: Some(other),
        ..BOUND
    };
    assert_eq!(check(&stale, &CORE), Err(LayoutError::Import));
    let stale_without = Carried {
        core: Some(other),
        ..GOOD
    };
    assert_eq!(check(&stale_without, &CORE), Err(LayoutError::Import));
    // A program with no import and no such record is the kernel's own kind, and passes.
    assert_eq!(check(&GOOD, &CORE), Ok(()));
}

#[test]
fn an_export_address_that_overflows_is_refused() {
    let top = CoreAt {
        base: u64::MAX - 0x10,
        image: &CORE,
    };
    assert_eq!(bound(&IMPORTS[0], top), None);
    let mut page = [0u8; PAGE as usize];
    assert_eq!(
        fill_page(&BOUND, 2, 0, 0, top, &mut page),
        Err(LayoutError::Import)
    );
    // And an import that names no export is no address at all.
    assert_eq!(
        bound(
            &Import {
                slot: 0x3008,
                export: 3
            },
            AT
        ),
        None
    );
}
