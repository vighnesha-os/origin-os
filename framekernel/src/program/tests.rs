//! The launch path's arithmetic on hand-made descriptions: [`check`]'s rules one by one, the
//! pages [`fill_page`] builds, and where [`random_base`] puts a program.

use super::*;

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
    assert_eq!(check(&GOOD), Ok(()));
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
        assert_eq!(check(&carried), Err(LayoutError::Segment), "case {i}");
    }
}

#[test]
fn exactly_one_code_segment_holds_the_entry() {
    let two = with_segments(segments(|s| s[1].access = SegmentAccess::ReadExecute));
    assert_eq!(check(&two), Err(LayoutError::Code));
    let none = with_segments(segments(|s| s[0].access = SegmentAccess::Read));
    assert_eq!(check(&none), Err(LayoutError::Code));
    for entry in [0x0FFF, 0x2000, 0x2010] {
        assert_eq!(check(&Carried { entry, ..GOOD }), Err(LayoutError::Code));
    }
}

#[test]
fn the_span_is_whole_pages_up_to_two_gib() {
    for span in [0, 0x6001, MAX_SPAN + PAGE] {
        assert_eq!(
            check(&Carried { span, ..GOOD }),
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
        assert_eq!(check(&carried), Err(LayoutError::Relocation), "{slots:x?}");
    }
}

#[test]
fn a_page_holds_its_bytes_with_the_base_added_to_its_slots() {
    let base = 0x0000_0123_4000_0000;
    let mut page = [0xFFu8; PAGE as usize];
    fill_page(&GOOD, 2, 0, base, &mut page).unwrap();
    assert_eq!(page[0..8], (0x2000 + base).to_le_bytes());
    assert_eq!(page[8..16], [0; 8]);
    assert_eq!(page[16..24], (0x1010 + base).to_le_bytes());
    assert!(page[24..].iter().all(|&b| b == 0), "zero after the bytes");

    // The writable data's first page: its bytes, and the slot at its last word.
    fill_page(&GOOD, 3, 0, base, &mut page).unwrap();
    assert!(page[..4088].iter().all(|&b| b == 0x11));
    assert_eq!(page[4088..], (0x3008 + base).to_le_bytes());
    // Its second page is all bss, and no slot of the page before leaks into it.
    fill_page(&GOOD, 3, 1, base, &mut page).unwrap();
    assert!(page.iter().all(|&b| b == 0));

    // Text is copied as it is: no slot is in it.
    fill_page(&GOOD, 0, 0, base, &mut page).unwrap();
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
        fill_page(&GOOD, 4, 0, 0, &mut page),
        Err(LayoutError::Segment)
    );
    assert_eq!(
        fill_page(&GOOD, 0, 1, 0, &mut page),
        Err(LayoutError::Segment)
    );
    assert_eq!(
        fill_page(&GOOD, 0, 0, 0, &mut page[..100]),
        Err(LayoutError::Segment)
    );
}

#[test]
fn a_base_that_overflows_a_slot_is_refused() {
    let mut page = [0u8; PAGE as usize];
    assert_eq!(
        fill_page(&GOOD, 2, 0, u64::MAX - 0x1000, &mut page),
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
