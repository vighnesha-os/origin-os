//! Seeded multi-fault soak of the container parser (ONBF §13 item 3; CLAUDE.md §72).
//!
//! `tests.rs` sweeps every single fault over one image. This file combines one to four
//! faults per case, over many image shapes, and judges each outcome independently of the
//! parser: an accepted image must re-encode to exactly its own bytes (ONBF-R6: nothing
//! unread, nothing unaccounted for) and satisfy every rule of §4.1–§4.4 and the errata,
//! each re-derived here from the specification.
//!
//! Faults are applied at three layers, so that the checks behind each layer are reached:
//! raw bytes (nothing re-sealed), header and table fields (content digest re-sealed), and
//! the section model (offsets, sizes, padding and digest all rebuilt by the builder).
//!
//! A failure is reproduced by its seed and case number; `ORIGIN_MUTATION_SCALE` scales the
//! case counts.

use crate::testing::{Reached, Rng, cases, mutate, mutate_window};
use crate::tests::{
    Builder, IMPORTS, MANIFEST, MAP, MERKLE, R, REQ, SEGMENT, STRINGS, Sec, W, X, align, dev,
    entry, kernel_with_manifest, nested, put16, put32, put64, sha256, sha512, strict,
};
use crate::tests_ocbor::krn_manifest_with_entry;
use crate::*;

const RESOURCES: u32 = 0x0003;
const RELOCS: u32 = 0x0011;
const EXPORTS: u32 = 0x0013;
const TLS: u32 = 0x0014;
const UNWIND: u32 = 0x0015;
const IMAGE_FS: u32 = 0x0020;
const PAYLOAD: u32 = 0x0030;
const FIRMWARE: u32 = 0x0040;
const DEBUG_LINK: u32 = 0x00F1;
const DWARF: u32 = 0x00F2;
const RELRO: u32 = Flags::RELRO;

const KINDS: [u32; 15] = [
    MANIFEST, STRINGS, RESOURCES, SEGMENT, RELOCS, IMPORTS, EXPORTS, TLS, UNWIND, IMAGE_FS,
    PAYLOAD, FIRMWARE, MERKLE, DEBUG_LINK, DWARF,
];

// ------------------------------------------------------------------ image shapes

/// Valid images that between them use every container feature, with the options that make
/// them parse. Each gets a share of the cases.
fn shapes() -> Vec<(Builder, ParseOptions)> {
    let profile = |p: Profile| p as u16;
    let mut all = vec![
        (Builder::kernel(), strict()),
        (Builder::kernel().unsigned(), dev()),
    ];

    // Parent coverage (integrity 3) in a nested image.
    let mut b = Builder::kernel().unsigned();
    b.sections[3].integrity = 3;
    b.sections[3].digest = [0; 32];
    all.push((b, nested()));

    // A kernel with a read-only segment, a RELRO segment and bss after a guard gap.
    let mut b = Builder::kernel();
    b.sections.push(Sec::plain(RELOCS, 24));
    b.sections
        .push(Sec::segment(R, 4096, 3 * 4096, 4096, NO_MERKLE));
    b.sections
        .push(Sec::segment(R | W | RELRO, 16, 4 * 4096, 4096, NO_MERKLE));
    b.sections
        .push(Sec::segment(R | W, 0, 6 * 4096, 2 * 4096, NO_MERKLE));
    all.push((b, strict()));

    // Compression, and Merkle protection on a section that is not mapped.
    let mut b = Builder::minimal(profile(Profile::App));
    let mut zipped = Sec::plain(STRINGS, 40);
    zipped.compression = 1;
    zipped.content_size = Some(5000);
    b.sections.push(zipped);
    b.sections.push(Sec::plain(RESOURCES, 13));
    let mut tree = Sec::plain(IMAGE_FS, 8193);
    tree.integrity = 1;
    tree.merkle_index = 4;
    tree.digest = [0x66; 32];
    b.sections.push(tree);
    b.sections.push(Sec::merkle(1));
    all.push((b, strict()));

    // A symbol file with compressed DWARF, a driver and an installer payload.
    let mut b = Builder::minimal(profile(Profile::Sym));
    let mut dwarf = Sec::plain(DWARF, 64);
    dwarf.compression = 1;
    dwarf.content_size = Some(1 << 20);
    b.sections.push(dwarf);
    b.sections.push(Sec::plain(DWARF, 17));
    all.push((b, strict()));
    let mut b = Builder::minimal(profile(Profile::Drv));
    b.sections.push(Sec::plain(FIRMWARE, 4097));
    b.sections.push(Sec::plain(IMAGE_FS, 8));
    all.push((b, strict()));
    let mut b = Builder::minimal(profile(Profile::Pkg));
    b.sections.push(Sec::plain(PAYLOAD, 300));
    all.push((b, strict()));

    // A library at the 16-segment limit, with an export table.
    let mut b = Builder::minimal(profile(Profile::Lib));
    for k in 0..16 {
        b.sections
            .push(Sec::segment(R | W, 0, k * 4096, 4096, NO_MERKLE));
    }
    b.sections.push(Sec::plain(EXPORTS, 40));
    all.push((b, strict()));

    // 31 sections: the table ends exactly on a page, so there is no table padding.
    let mut b = Builder::minimal(profile(Profile::Exe));
    b.sections
        .extend((0..30).map(|i| Sec::plain(STRINGS, 1 + i * 3)));
    all.push((b, strict()));

    for p in [1, 2, 3, 4, 5, 6, 7, 8] {
        all.push((Builder::minimal(p), strict()));
    }
    all
}

/// The 256-section image: built once and picked rarely, since it costs far more per case.
fn widest_shape() -> (Builder, ParseOptions) {
    let mut b = Builder::minimal(Profile::Exe as u16);
    b.sections
        .extend((0..255).map(|i| Sec::plain(STRINGS, 1 + i % 9)));
    (b, strict())
}

// ------------------------------------------------------------------ the three fault layers

/// A fault that leaves the image valid, so that a useful share of cases reach the
/// accepted-image checks with shapes the parser has not seen.
fn benign_fault(b: &mut Builder, rng: &mut Rng) {
    if b.sections.is_empty() {
        return;
    }
    let i = rng.below(b.sections.len());
    let s = &mut b.sections[i];
    let is_segment = s.kind == SEGMENT;
    match rng.below(6) {
        0 if s.integrity <= 1 => s.digest = [rng.byte(); 32],
        1 if is_segment && s.flags & W != 0 => s.flags ^= RELRO,
        // §6.1 allows these four, and no other.
        2 if is_segment => {
            s.flags =
                (s.flags & !(R | W | X | RELRO)) | rng.pick(&[R, R | W, R | W | RELRO, R | X]);
        }
        3 if s.kind != MERKLE && s.kind != MANIFEST && !is_segment && s.integrity == 0 => {
            let len = 1 + rng.below(9000);
            s.stored.resize(len, 0x77);
        }
        4 => {
            let top = b
                .sections
                .iter()
                .filter(|s| s.kind == SEGMENT)
                .map(|s| s.mem_offset.saturating_add(s.mem_size.unwrap_or(0)))
                .max();
            let segments_allowed = matches!(b.profile, 1 | 2 | 6);
            if let (Some(top), true) = (top, segments_allowed) {
                let gap = 4096 * rng.below(4) as u64;
                let pages = 4096 * (1 + rng.below(8) as u64);
                let rva = top.saturating_add(gap);
                b.sections
                    .push(Sec::segment(R | W, 0, rva, pages, NO_MERKLE));
            }
        }
        _ => {
            if let Some(sig) = &mut b.signature {
                let len = 1 + rng.below(2000);
                sig.resize(len, 0x5C);
            }
        }
    }
}

/// One fault in the section model. The builder then lays the file out canonically and
/// seals the content digest, so these faults reach the checks behind the layout.
fn model_fault(b: &mut Builder, rng: &mut Rng) {
    if b.sections.is_empty() {
        b.sections.push(Sec::manifest());
        return;
    }
    let n = b.sections.len();
    let i = rng.below(n);
    match rng.below(25) {
        0 => b.sections[i].kind = rng.pick(&KINDS),
        1 => b.sections[i].kind = rng.edge_u32(),
        2 => b.sections[i].flags ^= 1 << rng.below(8),
        3 => b.sections[i].flags = rng.pick(&[REQ, REQ | MAP, REQ | MAP | R, REQ | MAP | R | X]),
        4 => {
            let len = rng.pick(&[0, 1, 7, 8, 9, 4095, 4096, 4097, 8192, 8193]);
            b.sections[i].stored.resize(len, 0x77);
        }
        5 => b.sections[i].content_size = Some(rng.edge_u64()),
        6 => b.sections[i].content_size = Some((1 << 40) + rng.below(3) as u64),
        7 => b.sections[i].mem_offset = 4096 * rng.below(0x9_0000) as u64,
        8 => b.sections[i].mem_offset = rng.edge_u64(),
        9 => b.sections[i].mem_size = Some(4096 * rng.below(64) as u64),
        10 => b.sections[i].mem_size = Some(rng.edge_u64()),
        11 => b.sections[i].compression = rng.below(3) as u8,
        12 => b.sections[i].integrity = rng.below(5) as u8,
        13 => b.sections[i].merkle_index = rng.below(n + 2) as u32,
        14 => b.sections[i].merkle_index = rng.edge_u32(),
        15 => b.sections[i].digest = [if rng.chance(50) { 0 } else { rng.byte() }; 32],
        16 => {
            let extra = match rng.below(5) {
                0 => Sec::plain(rng.pick(&KINDS), 1 + rng.below(64)),
                1 => Sec::merkle(1 + rng.below(3)),
                2 => Sec::manifest(),
                3 => Sec::segment(R | X, 5000, 1 << 20, 8192, rng.below(n + 2) as u32),
                _ => Sec::segment(R | W, 0, 1 << 20, 4096, NO_MERKLE),
            };
            b.sections.push(extra);
        }
        17 => {
            b.sections.remove(i);
        }
        18 => b.sections.swap(i, rng.below(n)),
        19 => b.profile = rng.below(10) as u16,
        20 => b.signature = None,
        // Manifest rules (C12) that sit behind the integrity and digest checks: parent
        // coverage, or a manifest just over its size limit.
        21 => {
            let m = b.sections.iter_mut().find(|s| s.kind == MANIFEST);
            if let Some(m) = m {
                m.integrity = 3;
                m.digest = [0; 32];
            }
        }
        // A composite no single fault reaches: mapped, with the only integrity that allows.
        22 => {
            let s = &mut b.sections[i];
            s.flags |= MAP;
            s.integrity = rng.pick(&[1, 1, 3]);
            s.merkle_index = NO_MERKLE;
            if s.integrity == 3 {
                s.digest = [0; 32];
            }
        }
        23 if rng.below(16) == 0 => {
            let m = b.sections.iter_mut().find(|s| s.kind == MANIFEST);
            if let Some(m) = m {
                m.stored.resize((1 << 20) + rng.below(2), 0);
            }
        }
        _ => b.signature = Some(vec![0x5A; rng.pick(&[1, 8, 9, 64, 4096])]),
    }
}

/// The header and entry fields, as (offset, width), for field-level faults.
const HEADER_FIELDS: [(usize, usize); 10] = [
    (8, 2),
    (10, 2),
    (12, 2),
    (14, 2),
    (16, 4),
    (20, 4),
    (24, 8),
    (32, 8),
    (40, 8),
    (112, 2),
];
const ENTRY_FIELDS: [(usize, usize); 13] = [
    (0, 4),
    (4, 4),
    (8, 8),
    (16, 8),
    (24, 8),
    (32, 8),
    (40, 8),
    (48, 1),
    (49, 1),
    (50, 2),
    (52, 4),
    (56, 8),
    (96, 8),
];

/// Sets one header or section-entry field to a boundary-biased value.
fn field_fault(img: &mut [u8], rng: &mut Rng) {
    let count = declared_count(img);
    let (at, width) = if count == 0 || rng.chance(25) {
        rng.pick(&HEADER_FIELDS)
    } else {
        let (off, width) = rng.pick(&ENTRY_FIELDS);
        (entry(rng.below(count)) + off, width)
    };
    if at + width > img.len() {
        return;
    }
    let value = rng.edge_u64().to_le_bytes();
    img[at..at + width].copy_from_slice(&value[..width]);
}

/// The section count the header claims, when it is a plausible one and the table fits.
fn declared_count(img: &[u8]) -> usize {
    let Some(raw) = img.get(20..24) else {
        return 0;
    };
    let count = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
    if (1..=256).contains(&count) && 128 + 128 * count <= img.len() {
        count
    } else {
        0
    }
}

/// Recomputes `content_digest` over whatever the header and table now hold, so that the
/// digest does not hide what is behind it.
fn seal(img: &mut [u8]) {
    let table_end = 128 + 128 * declared_count(img);
    if img.len() >= 128 {
        let digest = sha512(&[&img[0..32], &img[112..128], &img[128..table_end.max(128)]]);
        img[48..112].copy_from_slice(&digest);
    }
}

// ------------------------------------------------------------------ what an outcome must be

fn section_error_name(e: SectionError) -> &'static str {
    use SectionError as S;
    match e {
        S::UnknownFlags(_) => "UnknownFlags",
        S::WriteAndExecute => "WriteAndExecute",
        S::PermissionsOnNonSegment => "PermissionsOnNonSegment",
        S::SegmentNotMappable => "SegmentNotMappable",
        S::SegmentPermissions(_) => "SegmentPermissions",
        S::MappableNotAllowed => "MappableNotAllowed",
        S::UnknownKind(_) => "UnknownKind",
        S::KindNotAllowedInProfile(_) => "KindNotAllowedInProfile",
        S::UnknownCompression(_) => "UnknownCompression",
        S::CompressionNotAllowed => "CompressionNotAllowed",
        S::TooLarge => "TooLarge",
        S::SizeMismatch => "SizeMismatch",
        S::EmptySection => "EmptySection",
        S::UnknownIntegrity(_) => "UnknownIntegrity",
        S::IntegrityNotAllowed(_) => "IntegrityNotAllowed",
        S::MerkleIndex(_) => "MerkleIndex",
        S::MerkleSize { .. } => "MerkleSize",
        S::MerkleReferences(_) => "MerkleReferences",
        S::DigestNotZero => "DigestNotZero",
        S::ManifestRules => "ManifestRules",
        S::RequiredNotSet => "RequiredNotSet",
        S::MemoryLayout => "MemoryLayout",
        S::SegmentOrder => "SegmentOrder",
        S::SegmentSpan => "SegmentSpan",
        S::Reserved => "Reserved",
        S::Misplaced { .. } => "Misplaced",
        S::OutOfBounds => "OutOfBounds",
    }
}

fn error_name(e: Error) -> &'static str {
    match e {
        Error::Truncated => "Truncated",
        Error::BadMagic => "BadMagic",
        Error::UnsupportedFormatMajor(_) => "UnsupportedFormatMajor",
        Error::UnknownProfile(_) => "UnknownProfile",
        Error::BadHeaderSize(_) => "BadHeaderSize",
        Error::HeaderFlags(_) => "HeaderFlags",
        Error::SectionCount(_) => "SectionCount",
        Error::SectionTableOffset(_) => "SectionTableOffset",
        Error::UnsupportedArch(_) => "UnsupportedArch",
        Error::HeaderReserved => "HeaderReserved",
        Error::SignatureBlockSize(_) => "SignatureBlockSize",
        Error::SignatureBlockMisplaced { .. } => "SignatureBlockMisplaced",
        Error::UnsignedNotAllowed => "UnsignedNotAllowed",
        Error::TrailingData => "TrailingData",
        Error::PaddingNotZero(_) => "PaddingNotZero",
        Error::ManifestCount => "ManifestCount",
        Error::TooManySegments => "TooManySegments",
        Error::Section { error, .. } => section_error_name(error),
        Error::ContentDigestMismatch => "ContentDigestMismatch",
        Error::NotSha256Section(_) => "NotSha256Section",
        Error::SectionDigestMismatch(_) => "SectionDigestMismatch",
    }
}

/// Every refusal the container soak must reach.
const IMAGE_REFUSALS: [&str; 48] = [
    "Truncated",
    "BadMagic",
    "UnsupportedFormatMajor",
    "UnknownProfile",
    "BadHeaderSize",
    "HeaderFlags",
    "SectionCount",
    "SectionTableOffset",
    "UnsupportedArch",
    "HeaderReserved",
    "SignatureBlockSize",
    "SignatureBlockMisplaced",
    "UnsignedNotAllowed",
    "TrailingData",
    "PaddingNotZero",
    "ManifestCount",
    "TooManySegments",
    "ContentDigestMismatch",
    "NotSha256Section",
    "SectionDigestMismatch",
    "UnknownFlags",
    "WriteAndExecute",
    "PermissionsOnNonSegment",
    "SegmentNotMappable",
    "SegmentPermissions",
    "MappableNotAllowed",
    "UnknownKind",
    "KindNotAllowedInProfile",
    "UnknownCompression",
    "CompressionNotAllowed",
    "TooLarge",
    "SizeMismatch",
    "EmptySection",
    "UnknownIntegrity",
    "IntegrityNotAllowed",
    "MerkleIndex",
    "MerkleSize",
    "MerkleReferences",
    "DigestNotZero",
    "ManifestRules",
    "RequiredNotSet",
    "MemoryLayout",
    "SegmentOrder",
    "SegmentSpan",
    "Reserved",
    "Misplaced",
    "OutOfBounds",
    "accepted",
];

/// The refusal's own claims about the bytes it names.
fn assert_refusal_is_truthful(case: usize, bytes: &[u8], e: Error) {
    match e {
        Error::PaddingNotZero(at) => assert_ne!(bytes[at as usize], 0, "case {case}"),
        Error::SignatureBlockMisplaced { expected, found } => {
            assert_ne!(expected, found, "case {case}");
        }
        Error::Section {
            index,
            error: SectionError::Misplaced { expected, found },
        } => {
            assert_ne!(expected, found, "case {case}: section {index}");
        }
        Error::Section {
            index,
            error: SectionError::MerkleSize { expected, found },
        } => {
            assert_ne!(expected, found, "case {case}: section {index}");
        }
        _ => {}
    }
}

/// The byte image an accepted [`Image`] describes, written from its parsed view alone.
/// Everything not written stays zero, which is exactly what padding must be.
fn reencode(image: &Image<'_>) -> Vec<u8> {
    let h = image.header();
    let table_end = 128 + 128 * image.section_count();
    let mut total = align(table_end, 4096);
    for s in image.sections() {
        total = total.max(s.entry.file_offset as usize + s.stored.len());
    }
    if let Some(sig) = image.signature_block() {
        total = total.max(h.signature_block_offset as usize + sig.len());
    }
    let mut out = vec![0u8; total];
    out[0..8].copy_from_slice(&MAGIC);
    put16(&mut out, 8, h.format_major);
    put16(&mut out, 10, h.format_minor);
    put16(&mut out, 12, h.profile as u16);
    put16(&mut out, 14, 128);
    put32(&mut out, 20, u32::from(h.section_count));
    put64(&mut out, 24, 128);
    put64(&mut out, 32, h.signature_block_offset);
    put64(&mut out, 40, h.signature_block_size);
    out[48..112].copy_from_slice(&h.content_digest);
    put16(&mut out, 112, ARCH_X86_64);
    for s in image.sections() {
        let e = entry(usize::from(s.index));
        let v = &s.entry;
        put32(&mut out, e, v.kind);
        put32(&mut out, e + 4, v.flags.0);
        put64(&mut out, e + 8, v.file_offset);
        put64(&mut out, e + 16, v.stored_size);
        put64(&mut out, e + 24, v.content_size);
        put64(&mut out, e + 32, v.mem_offset);
        put64(&mut out, e + 40, v.mem_size);
        out[e + 48] = match v.compression {
            Compression::None => 0,
            Compression::Zstd => 1,
        };
        out[e + 49] = v.integrity as u8;
        put32(&mut out, e + 52, v.merkle_index);
        out[e + 64..e + 96].copy_from_slice(&v.digest);
        let at = v.file_offset as usize;
        out[at..at + s.stored.len()].copy_from_slice(s.stored);
    }
    if let Some(sig) = image.signature_block() {
        let at = h.signature_block_offset as usize;
        out[at..at + sig.len()].copy_from_slice(sig);
    }
    out
}

/// The §4.4 table, from the specification rather than from the parser's bit sets.
fn kind_allowed_in(kind: u32, profile: Profile) -> bool {
    use Profile::*;
    match kind {
        MANIFEST | MERKLE => true,
        STRINGS | RESOURCES => matches!(profile, Exe | App | Pkg | Drv),
        SEGMENT | RELOCS | UNWIND | DEBUG_LINK => matches!(profile, Exe | Lib | Krn),
        IMPORTS | TLS => matches!(profile, Exe | Lib),
        EXPORTS => matches!(profile, Lib | Krn),
        IMAGE_FS => matches!(profile, App | Drv),
        PAYLOAD => profile == Pkg,
        FIRMWARE => profile == Drv,
        DWARF => profile == Sym,
        _ => false,
    }
}

/// §4.6, counted directly: the hash blocks a MERKLE section holds for `content` bytes.
fn hash_blocks(content: u64) -> u64 {
    let (mut hashes, mut total) = (content.div_ceil(4096), 0);
    while hashes > 1 {
        hashes = hashes.div_ceil(128);
        total += hashes;
    }
    total
}

/// Every rule of §4.1–§4.4 and the errata that an accepted image promises, re-derived here.
fn assert_rules(case: usize, bytes: &[u8], image: &Image<'_>, options: ParseOptions) {
    let h = image.header();
    let count = image.section_count();
    assert!((1..=256).contains(&count), "case {case}");
    assert_eq!(h.format_major, 1, "case {case}");
    assert_eq!(image.is_nested(), options.nested_in_verified_parent);

    // §4.2 and C4: the signature fields agree; unsigned needs a parent or developer mode.
    let signed = h.signature_block_offset != 0;
    assert_eq!(signed, h.signature_block_size != 0, "case {case}");
    assert!(h.signature_block_size <= 1 << 20, "case {case}");
    assert!(
        signed || options.nested_in_verified_parent || options.developer_mode,
        "case {case}: unsigned without a verified parent or developer mode"
    );

    let (mut manifests, mut segments, mut floor) = (0, 0, 0u64);
    let mut referrers = vec![0u32; count];
    for s in image.sections() {
        let (i, e) = (s.index, &s.entry);
        let k = e.kind();
        let mappable = e.flags.has(MAP);
        let segment = k == Kind::Segment;
        let at = format!("case {case}: section {i} {e:?}");

        // §4.3 flags, C6, G5, ONBF-R5.
        assert_eq!(e.flags.0 & !0x3F, 0, "{at}");
        assert!(e.flags.has(REQ), "{at}: REQUIRED not set (G5)");
        assert!(!e.flags.has(W | X), "{at}: W+X (ONBF-R5)");
        if segment {
            assert!(mappable, "{at}: a SEGMENT must be MAPPABLE (C6)");
            let permissions = e.flags.0 & (R | W | X | RELRO);
            assert!(
                [R, R | W, R | W | RELRO, R | X].contains(&permissions),
                "{at}: §6.1 allows R, RW (optionally RELRO) or RX"
            );
        } else {
            assert_eq!(e.flags.0 & (R | W | X | RELRO), 0, "{at}: permissions (C6)");
        }

        // §4.4 and C13.
        assert!(kind_allowed_in(e.kind, h.profile), "{at}: kind in profile");

        // §4.3 sizes and compression, Appendix A, C12, C9.
        assert!(
            e.stored_size <= 1 << 40 && e.content_size <= 1 << 40,
            "{at}"
        );
        assert_eq!(s.stored.len() as u64, e.stored_size, "{at}");
        match e.compression {
            Compression::None => assert_eq!(e.content_size, e.stored_size, "{at}"),
            Compression::Zstd => assert!(
                !mappable && k != Kind::Merkle && k != Kind::Manifest,
                "{at}: compression not allowed (§4.3, C9, C12)"
            ),
        }

        // C14: only a SEGMENT may be empty, and then only as bss.
        if segment {
            assert!(e.mem_size > 0, "{at}: (C14)");
        } else {
            assert!(e.content_size > 0, "{at}: (C14)");
        }

        // Integrity: C8, C9, C11, C12.
        assert_eq!(
            k == Kind::Merkle,
            e.integrity == Integrity::MerkleHashBlocks,
            "{at}: (C9)"
        );
        if mappable {
            assert!(
                matches!(e.integrity, Integrity::Merkle | Integrity::CoveredByParent),
                "{at}: a MAPPABLE section uses integrity 1 or 3 (C8)"
            );
        }
        if e.integrity == Integrity::CoveredByParent {
            assert!(options.nested_in_verified_parent, "{at}: (C11)");
        }
        if k == Kind::Manifest {
            manifests += 1;
            assert_eq!(e.integrity, Integrity::Sha256, "{at}: (C12)");
            assert!(e.content_size <= 1 << 20, "{at}: (C12)");
        }

        // G1: a DWARF section is never mapped.
        if k == Kind::Dwarf {
            assert!(!mappable, "{at}: a DWARF section is never MAPPABLE (G1)");
        }

        // The digest and merkle_index fields (C9, C10, C11).
        if matches!(
            e.integrity,
            Integrity::MerkleHashBlocks | Integrity::CoveredByParent
        ) {
            assert_eq!(e.digest, [0; 32], "{at}");
        }
        if e.integrity == Integrity::Merkle {
            let blocks = hash_blocks(e.content_size);
            if blocks == 0 {
                assert_eq!(e.merkle_index, NO_MERKLE, "{at}: (C10)");
            } else {
                let target = e.merkle_index as usize;
                let m = image.section(target).expect("merkle_index names a section");
                assert_eq!(m.entry.kind(), Kind::Merkle, "{at}");
                assert_eq!(m.entry.stored_size, blocks * 4096, "{at}: (C10)");
                referrers[target] += 1;
            }
        } else {
            assert_eq!(e.merkle_index, NO_MERKLE, "{at}");
        }

        // Memory placement: C7, G4, Appendix A.
        if segment {
            segments += 1;
            assert!(e.mem_offset.is_multiple_of(4096), "{at}: RVA (G4)");
            assert!(e.mem_size.is_multiple_of(4096), "{at}: size (G4)");
            assert!(e.mem_size >= e.content_size, "{at}");
            assert!(e.mem_offset >= floor, "{at}: order or overlap (G4)");
            floor = e.mem_offset + e.mem_size;
            assert!(floor <= 1 << 31, "{at}: span (G4)");
        } else {
            assert_eq!(
                (e.mem_offset, e.mem_size),
                (0, e.content_size),
                "{at}: (C7)"
            );
        }
    }
    assert_eq!(manifests, 1, "case {case}: §4.4");
    if matches!(h.profile, Profile::Exe | Profile::Lib) {
        assert!(segments <= 16, "case {case}: Appendix A");
    }
    for s in image.sections() {
        if s.entry.kind() == Kind::Merkle {
            assert_eq!(
                referrers[usize::from(s.index)],
                1,
                "case {case}: MERKLE section {} must serve exactly one section (C9)",
                s.index
            );
        }
    }

    // §4.1, C1–C3: each item at its canonical position, the last ending exactly at EOF.
    let mut pos = align(128 + 128 * count, 4096);
    for s in image.sections() {
        let a = if s.entry.flags.has(MAP) { 4096 } else { 8 };
        let want = align(pos, a);
        assert_eq!(s.entry.file_offset, want as u64, "case {case}: C1");
        pos = want + s.stored.len();
    }
    match image.signature_block() {
        None => assert_eq!(pos, bytes.len(), "case {case}: EOF"),
        Some(sig) => {
            let at = h.signature_block_offset as usize;
            assert_eq!(at, align(pos, 8), "case {case}: C3");
            assert_eq!(at + sig.len(), bytes.len(), "case {case}: EOF");
            assert_eq!(sig.len() as u64, h.signature_block_size, "case {case}");
        }
    }
}

/// A sample of padding offsets: bytes of an accepted image that must stay zero.
fn padding_offsets(image: &Image<'_>, bytes_len: usize) -> Vec<(usize, usize)> {
    let table_end = 128 + 128 * image.section_count();
    let mut out = Vec::new();
    let mut pos = align(table_end, 4096);
    out.push((table_end, pos));
    for s in image.sections() {
        out.push((pos, s.entry.file_offset as usize));
        pos = s.entry.file_offset as usize + s.stored.len();
    }
    if image.signature_block().is_some() {
        out.push((pos, image.header().signature_block_offset as usize));
    }
    out.retain(|&(a, b)| a < b && b <= bytes_len);
    out
}

/// Everything an accepted image must satisfy, plus the probes that only make sense on one.
fn assert_accepted(
    case: usize,
    bytes: &[u8],
    image: &Image<'_>,
    options: ParseOptions,
    rng: &mut Rng,
    reached: &mut Reached,
) {
    reached.hit("accepted");
    // ONBF-R6: every byte is accounted for, every field read from the right place.
    assert!(
        reencode(image) == bytes,
        "case {case}: an accepted image does not re-encode to itself"
    );
    assert_rules(case, bytes, image, options);

    // Accessors agree, and stop at the end of the table.
    let count = image.section_count();
    assert_eq!(image.sections().count(), count, "case {case}");
    assert!(image.section(count).is_none(), "case {case}");
    let manifest = image.manifest().expect("exactly one manifest");
    assert_eq!(manifest.entry.kind(), Kind::Manifest, "case {case}");

    // The rest re-hashes and re-parses the whole image; a sample keeps the soak fast, and
    // the sample still covers thousands of distinct shapes at the default scale.
    if rng.below(3) != 0 {
        return;
    }

    // §4.5: the digest covers exactly the three ranges, compared independently.
    let table_end = 128 + 128 * count;
    let want = sha512(&[&bytes[0..32], &bytes[112..128], &bytes[128..table_end]]);
    let result = image.verify_content_digest(sha512);
    assert_eq!(result.is_ok(), want == bytes[48..112], "case {case}");
    if let Err(e) = result {
        reached.hit(error_name(e));
    }
    for s in image.sections() {
        let plain =
            s.entry.integrity == Integrity::Sha256 && s.entry.compression == Compression::None;
        let result = s.verify_sha256(sha256);
        match (plain, sha256(s.stored) == s.entry.digest) {
            (false, _) => assert_eq!(result, Err(Error::NotSha256Section(s.index)), "case {case}"),
            (true, true) => assert_eq!(result, Ok(()), "case {case}"),
            (true, false) => {
                assert_eq!(
                    result,
                    Err(Error::SectionDigestMismatch(s.index)),
                    "case {case}"
                );
            }
        }
        if let Err(e) = result {
            reached.hit(error_name(e));
        }
    }

    // ONBF-R1: the verdict depends only on the header, the table and the padding, so
    // scrambling every section's content and the signature block changes nothing.
    let mut scrambled = bytes.to_vec();
    let flip = rng.byte() | 1;
    for s in image.sections() {
        let at = s.entry.file_offset as usize;
        scrambled[at..at + s.stored.len()]
            .iter_mut()
            .for_each(|b| *b ^= flip);
    }
    if let Some(sig) = image.signature_block() {
        let at = image.header().signature_block_offset as usize;
        scrambled[at..at + sig.len()]
            .iter_mut()
            .for_each(|b| *b ^= flip);
    }
    let again = Image::parse(&scrambled, options)
        .unwrap_or_else(|e| panic!("case {case}: the verdict depended on section content: {e:?}"));
    assert_eq!(again.header(), image.header(), "case {case}");

    // Any padding byte, on any accepted layout, is refused (C2), and so is a file one byte
    // shorter or longer, or a random prefix (C1, ONBF-R6).
    let padding = padding_offsets(image, bytes.len());
    if !padding.is_empty() {
        let (a, b) = rng.pick(&padding);
        let at = a + rng.below(b - a);
        let mut poked = bytes.to_vec();
        poked[at] = 1 + rng.byte() % 0xFF;
        assert_eq!(
            Image::parse(&poked, options).err(),
            Some(Error::PaddingNotZero(at as u64)),
            "case {case}: padding byte {at}"
        );
    }
    let cut = rng.below(bytes.len());
    assert!(
        Image::parse(&bytes[..cut], options).is_err(),
        "case {case}: cut {cut}"
    );
    assert!(
        Image::parse(&bytes[..bytes.len() - 1], options).is_err(),
        "case {case}"
    );
    let mut longer = bytes.to_vec();
    longer.push(0);
    assert!(
        Image::parse(&longer, options).is_err(),
        "case {case}: a longer file"
    );
}

// ------------------------------------------------------------------ the soaks

/// `IMAGE_SEED`, case `n`, reproduces case `n` of the soak below.
const IMAGE_SEED: u64 = 0x4F4E_4246_494D_4731; // "ONBFIMG1"

/// Multi-fault soak of `Image::parse`. Never panics (overflow checks are on in test builds).
/// Every outcome is either a refusal that is truthful about the bytes it names, or an image
/// that re-encodes to its own bytes and obeys every rule of §4.
#[test]
fn mutated_images_are_refused_or_keep_every_section_4_rule() {
    let shapes = shapes();
    let widest = widest_shape();
    let mut reached = Reached::default();
    let total = cases(12_000);
    for case in 0..total {
        let mut rng = Rng::for_case(IMAGE_SEED, case);
        let (shape, shape_options) = if rng.below(128) == 0 {
            &widest
        } else {
            &shapes[rng.below(shapes.len())]
        };
        let mut b = shape.clone();
        let options = match rng.below(16) {
            0 => strict(),
            1 => dev(),
            2 => nested(),
            _ => *shape_options,
        };

        // Which layer the faults go in, and whether what they would break is re-sealed.
        let mode = rng.below(10);
        let mut model_only = false;
        let mut bytes = match mode {
            // Raw bytes anywhere in the file; nothing re-sealed.
            0 | 1 => {
                let mut img = b.build();
                mutate(&mut img, &mut rng);
                img
            }
            // Header and table bytes, then the digest re-sealed.
            2 | 3 => {
                let mut img = b.build();
                let end = (128 + 128 * b.sections.len()).min(img.len());
                mutate_window(&mut img, 0..end, &mut rng);
                seal(&mut img);
                img
            }
            // Single fields set to boundary values, then the digest re-sealed.
            4 | 5 => {
                let mut img = b.build();
                for _ in 0..1 + rng.below(4) {
                    field_fault(&mut img, &mut rng);
                }
                if rng.chance(75) {
                    seal(&mut img);
                }
                img
            }
            // The model, so offsets, sizes, padding and digest stay canonical.
            6..=8 => {
                model_only = true;
                for _ in 0..1 + rng.below(4) {
                    if rng.chance(30) {
                        benign_fault(&mut b, &mut rng);
                    } else {
                        model_fault(&mut b, &mut rng);
                    }
                }
                b.build()
            }
            // Only benign faults, so accepted shapes keep coming.
            _ => {
                model_only = true;
                for _ in 0..1 + rng.below(4) {
                    benign_fault(&mut b, &mut rng);
                }
                b.build()
            }
        };

        let result = Image::parse(&bytes, options);
        match &result {
            Err(e) => {
                reached.hit(error_name(*e));
                assert_refusal_is_truthful(case, &bytes, *e);
            }
            Ok(image) => assert_accepted(case, &bytes, image, options, &mut rng, &mut reached),
        }

        // ONBF-R1 for refusals too: the layout is the model's, so scrambling the content
        // and the signature block must not change the verdict either.
        if model_only && !b.sections.is_empty() {
            let flip = rng.byte() | 1;
            for s in &mut b.sections {
                s.stored.iter_mut().for_each(|x| *x ^= flip);
            }
            if let Some(sig) = &mut b.signature {
                sig.iter_mut().for_each(|x| *x ^= flip);
            }
            let verdict = result.as_ref().map(|i| *i.header()).map_err(|e| *e);
            bytes = b.build();
            let other = Image::parse(&bytes, options);
            assert_eq!(
                other.map(|i| *i.header()),
                verdict,
                "case {case}: the verdict depended on section content"
            );
        }
    }
    reached.require("container soak", &IMAGE_REFUSALS);
    assert!(
        reached.count("accepted") >= total as u64 / 8,
        "only {} of {total} cases were accepted",
        reached.count("accepted")
    );
}

/// `LOADER_SEED`, case `n`, reproduces case `n` of the soak below.
const LOADER_SEED: u64 = 0x4F4E_4246_4C4F_4144; // "ONBFLOAD"

/// The loader's order (ONBF-R1) under multi-fault input: each layer sees input only after
/// the layer before it passed, and re-sealing lets faults reach the manifest decoder and
/// the entry-point check.
#[test]
fn mutated_kernels_follow_the_loader_order() {
    let mut reached = Reached::default();
    let total = cases(4000);
    for case in 0..total {
        let mut rng = Rng::for_case(LOADER_SEED, case);
        let entry_rva = rng.pick(&[0u64, 0x100, 4999, 5000, 8192, u64::MAX]);
        let mut doc = krn_manifest_with_entry(entry_rva);
        let sealed_manifest = rng.chance(80);
        if rng.chance(50) {
            mutate(&mut doc, &mut rng);
        }
        let mut b = kernel_with_manifest(doc.clone());
        if !sealed_manifest {
            b.sections[0].digest[rng.below(32)] ^= 1 << rng.below(8);
        }
        if rng.chance(40) {
            for _ in 0..1 + rng.below(3) {
                model_fault(&mut b, &mut rng);
            }
        }
        // The manifest's profile rule sits behind everything else.
        if rng.chance(8) {
            b.profile = rng.pick(&[Profile::Exe as u16, Profile::Lib as u16]);
        }
        let mut bytes = b.build();
        if rng.chance(15) {
            let at = 48 + rng.below(64);
            bytes[at] ^= 1 << rng.below(8);
        }

        let Ok(image) = Image::parse(&bytes, strict()) else {
            reached.hit("container refused");
            continue;
        };
        if image.verify_content_digest(sha512).is_err() {
            reached.hit("content digest");
            continue;
        }
        let manifest = image.manifest().expect("exactly one manifest");
        if manifest.verify_sha256(sha256).is_err() {
            reached.hit("manifest digest");
            continue;
        }
        let krn = match KrnManifest::decode(manifest.stored) {
            Ok(krn) => krn,
            Err(_) => {
                reached.hit("manifest refused");
                continue;
            }
        };
        // check_against, judged by its own definition.
        let in_rx_content = image.sections().any(|s| {
            let e = &s.entry;
            e.kind() == Kind::Segment
                && e.flags.has(R | X)
                && u128::from(krn.entry) >= u128::from(e.mem_offset)
                && u128::from(krn.entry) < u128::from(e.mem_offset) + u128::from(e.content_size)
        });
        let want = if image.header().profile != Profile::Krn {
            Err(ManifestError::NotKrn)
        } else if in_rx_content {
            Ok(())
        } else {
            Err(ManifestError::EntryNotExecutable(krn.entry))
        };
        assert_eq!(krn.check_against(&image), want, "case {case}");
        match want {
            Ok(()) => reached.hit("kernel accepted"),
            Err(ManifestError::NotKrn) => reached.hit("not a kernel"),
            Err(_) => reached.hit("entry not in code"),
        }
    }
    reached.require(
        "loader-order soak",
        &[
            "container refused",
            "content digest",
            "manifest digest",
            "manifest refused",
            "not a kernel",
            "entry not in code",
            "kernel accepted",
        ],
    );
    assert!(
        reached.count("kernel accepted") >= total as u64 / 20,
        "{}",
        reached.count("kernel accepted")
    );
}
