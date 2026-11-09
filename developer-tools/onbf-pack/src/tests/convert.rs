//! Valid conversions, each checked independently of `krn::verify`: with `onbf-format`
//! (container, digests, manifest), `onbf-merkle` (roots, MERKLE sections and per-block
//! verification) and the RELR decoder.

use onbf_format::{
    Compression, Flags, Image, Integrity, Kind, KrnManifest, NO_MERKLE, ParseOptions, Profile,
    Section,
};
use onbf_merkle::{NoCache, Shape};
use origin_crypto::{Sha256, Sha512};

use super::elf_writer::{
    Load, PT_DYNAMIC, PT_GNU_STACK, PT_LOAD, Phdr, R, RELATIVE, RW, RX, RawElf, TestElf, text,
};
use super::options;
use crate::krn::{Packed, pack, verify};
use crate::relr;

/// What an independent reader finds in a KRN image.
struct Inspected<'a> {
    image: Image<'a>,
    manifest: KrnManifest<'a>,
    segments: Vec<Section<'a>>,
    relocations: Option<Vec<u64>>,
    merkle_sections: usize,
}

fn sha512(parts: &[&[u8]; 3]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finalize()
}

/// Reads `bytes` the way the boot loader will, and checks every rule of the brief.
fn inspect(bytes: &[u8]) -> Inspected<'_> {
    let options = ParseOptions {
        developer_mode: true,
        ..ParseOptions::default()
    };
    let image = Image::parse(bytes, options).expect("onbf-format accepts the image");
    image.verify_content_digest(sha512).unwrap();
    // Unsigned images are rejected outside developer mode.
    assert!(Image::parse(bytes, ParseOptions::default()).is_err());

    let h = image.header();
    assert_eq!(h.format_major, 1);
    assert_eq!(h.format_minor, origin_version::ONBF_FORMAT_MINOR);
    assert_eq!(h.profile, Profile::Krn);
    assert_eq!((h.signature_block_offset, h.signature_block_size), (0, 0));
    // Header reserved bytes and arch.
    assert_eq!(&bytes[112..114], &1u16.to_le_bytes());
    assert!(bytes[114..128].iter().all(|&b| b == 0));
    assert!(bytes[16..20].iter().all(|&b| b == 0));

    for s in image.sections() {
        assert!(s.entry.flags.has(Flags::REQUIRED), "section {}", s.index);
        assert_eq!(s.entry.compression, Compression::None);
    }
    let manifest_section = image.manifest().unwrap();
    assert_eq!(manifest_section.index, 0);
    assert_eq!(manifest_section.entry.integrity, Integrity::Sha256);
    manifest_section.verify_sha256(Sha256::digest).unwrap();
    let manifest = KrnManifest::decode(manifest_section.stored).unwrap();
    manifest.check_against(&image).unwrap();

    let segments: Vec<Section<'_>> = image
        .sections()
        .filter(|s| s.entry.kind() == Kind::Segment)
        .collect();
    let mut build_id = Sha256::new();
    for s in &segments {
        let e = &s.entry;
        assert!(e.flags.has(Flags::MAPPABLE | Flags::READ));
        assert!(!e.flags.has(Flags::WRITE | Flags::EXECUTE));
        assert_eq!(e.integrity, Integrity::Merkle);
        assert_eq!(e.mem_size % 4096, 0);
        assert_eq!(onbf_merkle::root(s.stored).unwrap(), e.digest);
        let shape = Shape::new(e.content_size).unwrap();
        let hash_blocks: &[u8] = if shape.hash_blocks() == 0 {
            assert_eq!(e.merkle_index, NO_MERKLE);
            &[]
        } else {
            let m = image.section(e.merkle_index as usize).unwrap();
            assert_eq!(m.entry.kind(), Kind::Merkle);
            assert!(m.index > s.index);
            let mut expected = vec![0u8; shape.hash_blocks_len() as usize];
            onbf_merkle::build(s.stored, &mut expected).unwrap();
            assert_eq!(m.stored, &expected[..]);
            m.stored
        };
        // Every page verifies on its own, as on a page fault (ONBF-R2).
        for (i, block) in s.stored.chunks(4096).enumerate() {
            onbf_merkle::verify_block(
                &e.digest,
                &shape,
                hash_blocks,
                i as u64,
                block,
                &mut NoCache,
            )
            .unwrap();
        }
        build_id.update(s.stored);
    }
    assert_eq!(build_id.finalize(), manifest.build_id);

    let relocations = image
        .sections()
        .find(|s| s.entry.kind() == Kind::Relocs)
        .map(|r| {
            assert_eq!(r.entry.integrity, Integrity::Sha256);
            assert!(!r.entry.flags.has(Flags::MAPPABLE));
            r.verify_sha256(Sha256::digest).unwrap();
            relr::decode(&relr::words_from_bytes(r.stored).unwrap()).unwrap()
        });
    let merkle_sections = image
        .sections()
        .filter(|s| s.entry.kind() == Kind::Merkle)
        .count();
    Inspected {
        image,
        manifest,
        segments,
        relocations,
        merkle_sections,
    }
}

fn packed(elf: &TestElf) -> Packed {
    pack(&elf.bytes(), &options()).expect("the ELF converts")
}

fn kinds(image: &Image<'_>) -> Vec<Kind> {
    image.sections().map(|s| s.entry.kind()).collect()
}

fn u64_at(content: &[u8], at: u64) -> u64 {
    u64::from_le_bytes(content[at as usize..at as usize + 8].try_into().unwrap())
}

#[test]
fn without_relocations() {
    let p = packed(&TestElf::kernel());
    let k = inspect(&p.bytes);
    assert_eq!(
        kinds(&k.image),
        [Kind::Manifest, Kind::Segment, Kind::Segment, Kind::Segment]
    );
    assert_eq!((p.sections, p.segments, p.relocations), (4, 3, 0));
    assert!(k.relocations.is_none());
    assert_eq!(k.merkle_sections, 0);

    // Placement: RVAs from p_vaddr, sizes from p_memsz rounded up to pages (G4).
    let placed: Vec<(u64, u64, u64)> = k
        .segments
        .iter()
        .map(|s| (s.entry.mem_offset, s.entry.mem_size, s.entry.content_size))
        .collect();
    assert_eq!(
        placed,
        [
            (0x1000, 0x1000, 64),
            (0x2000, 0x1000, 21),
            (0x3000, 0x1000, 64)
        ]
    );
    let perms: Vec<u32> = k
        .segments
        .iter()
        .map(|s| s.entry.flags.0 & (Flags::READ | Flags::WRITE | Flags::EXECUTE))
        .collect();
    assert_eq!(
        perms,
        [
            Flags::READ | Flags::EXECUTE,
            Flags::READ,
            Flags::READ | Flags::WRITE
        ]
    );
    // Content is the file bytes.
    assert_eq!(k.segments[0].stored, &text(64)[..]);
    assert_eq!(k.segments[1].stored, b"origin read-only data");

    // The manifest (docs/spec/ONBF-manifest-keys.md).
    let m = &k.manifest;
    let o = options();
    assert_eq!(m.entry, 0x1000);
    assert_eq!(m.cpu_features.0, 0);
    assert!(m.cet.shstk && !m.cet.ibt);
    assert_eq!(
        m.boot_info_abi,
        (
            origin_version::BOOT_INFO_MAJOR,
            origin_version::BOOT_INFO_MINOR
        )
    );
    assert_eq!(m.toolchain.rustc, o.toolchain.rustc);
    assert_eq!(m.toolchain.rustc_commit, o.toolchain.rustc_commit);
    assert_eq!(m.toolchain.llvm, o.toolchain.llvm);
    assert_eq!(m.toolchain.flags_digest, Sha256::digest(o.flags.as_bytes()));

    // Canonical offsets (C1): 4 entries end at 640, so the manifest is at 4096, and each
    // segment on the next page.
    let offsets: Vec<u64> = k.image.sections().map(|s| s.entry.file_offset).collect();
    assert_eq!(offsets, [4096, 8192, 12288, 16384]);
    assert_eq!(p.bytes.len(), 16384 + 64);
}

#[test]
fn with_rela_relocations() {
    let mut elf = TestElf::kernel();
    // Out of order and with one exact duplicate, as a linker might emit.
    elf.rela = vec![
        (0x3010, RELATIVE, 0x2005),
        (0x3000, RELATIVE, 0x1000),
        (0x3038, RELATIVE, 0xFFFF_FFFF_FFFF_FFF0), // a negative addend
        (0x3010, RELATIVE, 0x2005),
    ];
    let p = packed(&elf);
    let k = inspect(&p.bytes);
    assert_eq!(
        kinds(&k.image),
        [
            Kind::Manifest,
            Kind::Segment,
            Kind::Segment,
            Kind::Segment,
            Kind::Segment,
            Kind::Relocs
        ]
    );
    assert_eq!(p.relocations, 3);
    assert_eq!(
        k.relocations.as_deref(),
        Some(&[0x3000, 0x3010, 0x3038][..])
    );
    // The addends are now implicit: in the slots of the RW segment's content.
    let rw = &k.segments[2];
    assert_eq!(rw.entry.mem_offset, 0x3000);
    assert_eq!(u64_at(rw.stored, 0x00), 0x1000);
    assert_eq!(u64_at(rw.stored, 0x10), 0x2005);
    assert_eq!(u64_at(rw.stored, 0x38), 0xFFFF_FFFF_FFFF_FFF0);
    // Every other byte is unchanged.
    let mut expected = [0u8; 64];
    expected[0..8].copy_from_slice(&0x1000u64.to_le_bytes());
    expected[0x10..0x18].copy_from_slice(&0x2005u64.to_le_bytes());
    expected[0x38..0x40].copy_from_slice(&0xFFFF_FFFF_FFFF_FFF0u64.to_le_bytes());
    assert_eq!(rw.stored, &expected[..]);
    // The table segment (RELA and dynamic) is carried as an R segment.
    assert_eq!(k.segments[3].entry.mem_offset, 0x4000);
    assert_eq!(k.segments[3].entry.flags.0 & Flags::WRITE, 0);
    // RELOCS: one address word and one bitmap. Its window starts at 0x3008, so 0x3010
    // and 0x3038 are bits 1 and 6.
    let relocs = k.image.sections().last().unwrap();
    assert_eq!(
        relr::words_from_bytes(relocs.stored).unwrap(),
        [0x3000, (1 << 1 | 1 << 6) << 1 | 1]
    );
    assert_eq!(relocs.entry.file_offset % 8, 0);
}

#[test]
fn with_relr_relocations() {
    let mut elf = TestElf::kernel();
    let slots = [0x3000, 0x3008, 0x3018];
    elf.relr = relr::encode(&slots).unwrap();
    // RELR addends are already in place; they must survive unchanged.
    elf.loads[2].data[0..8].copy_from_slice(&0x1111u64.to_le_bytes());
    elf.loads[2].data[0x18..0x20].copy_from_slice(&0x2222u64.to_le_bytes());
    let data = elf.loads[2].data.clone();
    let p = packed(&elf);
    let k = inspect(&p.bytes);
    assert_eq!(k.relocations.as_deref(), Some(&slots[..]));
    assert_eq!(k.segments[2].stored, &data[..]);
}

#[test]
fn rela_and_relr_are_merged() {
    let mut elf = TestElf::kernel();
    elf.relr = relr::encode(&[0x3008, 0x3020]).unwrap();
    elf.rela = vec![(0x3000, RELATIVE, 0x1000), (0x3030, RELATIVE, 0x2000)];
    let p = packed(&elf);
    let k = inspect(&p.bytes);
    assert_eq!(
        k.relocations.as_deref(),
        Some(&[0x3000, 0x3008, 0x3020, 0x3030][..])
    );
    assert_eq!(u64_at(k.segments[2].stored, 0x30), 0x2000);
}

#[test]
fn bss_only_segment() {
    let mut elf = TestElf::kernel();
    elf.loads
        .push(Load::new(RW, 0x5000, Vec::new()).mem(0x2001));
    let p = packed(&elf);
    let k = inspect(&p.bytes);
    let bss = &k.segments[3];
    assert_eq!(bss.entry.content_size, 0);
    assert_eq!(bss.entry.stored_size, 0);
    assert_eq!(bss.entry.mem_offset, 0x5000);
    assert_eq!(bss.entry.mem_size, 0x3000);
    assert_eq!(bss.entry.merkle_index, NO_MERKLE);
    assert_eq!(bss.entry.digest, onbf_merkle::root(&[]).unwrap());
    assert_eq!(bss.entry.file_offset % 4096, 0);
    // The image ends exactly at the empty segment's canonical offset.
    assert_eq!(bss.entry.file_offset, p.bytes.len() as u64);
}

#[test]
fn large_segments_get_merkle_sections() {
    let mut elf = TestElf::kernel();
    // 5000 bytes: 2 blocks, 1 hash block. 129 blocks + 1 byte: 2 levels, 3 hash blocks.
    elf.loads[0] = Load::new(RX, 0x1000, text(5000));
    let big: Vec<u8> = (0..129 * 4096 + 1).map(|i| (i % 251) as u8).collect();
    elf.loads[1] = Load::new(R, 0x3000, big.clone());
    elf.loads[2].vaddr = 0x90000;
    elf.rela = vec![(0x90008, RELATIVE, 0x3000)];
    let p = packed(&elf);
    let k = inspect(&p.bytes);
    assert_eq!(
        kinds(&k.image),
        [
            Kind::Manifest,
            Kind::Segment,
            Kind::Segment,
            Kind::Segment,
            Kind::Segment,
            Kind::Relocs,
            Kind::Merkle,
            Kind::Merkle
        ]
    );
    assert_eq!(k.merkle_sections, 2);
    assert_eq!(k.segments[0].entry.merkle_index, 6);
    assert_eq!(k.segments[1].entry.merkle_index, 7);
    assert_eq!(k.segments[2].entry.merkle_index, NO_MERKLE);
    assert_eq!(k.image.section(6).unwrap().entry.stored_size, 4096);
    assert_eq!(k.image.section(7).unwrap().entry.stored_size, 3 * 4096);
    assert_eq!(k.segments[1].stored, &big[..]);
    // MERKLE sections are not mappable and sit at 8-byte-aligned offsets after RELOCS.
    for i in [6, 7] {
        let m = k.image.section(i).unwrap();
        assert!(!m.entry.flags.has(Flags::MAPPABLE));
        assert_eq!(m.entry.integrity, Integrity::MerkleHashBlocks);
        assert_eq!(m.entry.digest, [0; 32]);
    }
}

#[test]
fn cpu_features_and_flags_reach_the_manifest() {
    let mut o = options();
    o.cpu_features = 0x3F;
    o.flags = String::new();
    let p = pack(&TestElf::kernel().bytes(), &o).unwrap();
    let k = inspect(&p.bytes);
    assert_eq!(k.manifest.cpu_features.0, 0x3F);
    assert_eq!(k.manifest.toolchain.flags_digest, Sha256::digest(b""));
}

#[test]
fn packing_is_deterministic() {
    let mut elf = TestElf::kernel();
    // 9000 bytes of text (3 pages) at 0x1000, so the other segments move up.
    elf.loads[0] = Load::new(RX, 0x1000, text(9000));
    elf.loads[1].vaddr = 0x4000;
    elf.loads[2].vaddr = 0x5000;
    elf.rela = (0..8)
        .map(|i| (0x5000 + 8 * i, RELATIVE, 0x4000 + i))
        .collect();
    let bytes = elf.bytes();
    // Two independent runs over separately allocated copies of the input.
    let a = pack(&bytes, &options()).unwrap();
    let b = pack(&bytes.clone(), &options()).unwrap();
    assert_eq!(a.bytes, b.bytes);
    let v = verify(&a.bytes).unwrap();
    assert_eq!(
        v.relocations,
        (0..8).map(|i| 0x5000 + 8 * i).collect::<Vec<_>>()
    );
    // Different options change the image, so the comparison above is meaningful.
    let mut o = options();
    o.flags.push(' ');
    assert_ne!(pack(&bytes, &o).unwrap().bytes, a.bytes);
}

#[test]
fn tampering_is_detected_by_verify() {
    let mut elf = TestElf::kernel();
    elf.rela = vec![(0x3000, RELATIVE, 0x1000)];
    let good = packed(&elf).bytes;
    verify(&good).unwrap();
    // Flip one bit in every byte position of the file: verify must never accept a
    // different image.
    for i in 0..good.len() {
        let mut bad = good.clone();
        bad[i] ^= 0x10;
        assert!(verify(&bad).is_err(), "byte {i} flipped and still verified");
    }
}

/// The real Framekernel's shape, as the coordinator reported it on 2026-11-07 from the
/// rust-lld output of `framekernel` (x86_64-unknown-none, release): R at 0, R+X at 0x1000
/// of 0x2253 bytes, RW at 0x4000 with filesz 0x2E0 and memsz 0x2E1; PT_PHDR,
/// PT_GNU_EH_FRAME, PT_DYNAMIC (in the RW segment) and PT_GNU_STACK; 38 RELATIVE relocations
/// in DT_RELA (912 bytes), with DT_FLAGS, DT_FLAGS_1, DT_DEBUG, the symbol and hash tables.
fn framekernel_shape() -> (Vec<u8>, Vec<(u64, u64)>) {
    let mut raw = RawElf::new();
    raw.entry = 0x14F0;
    // R segment: ELF header, program headers, .dynsym (null symbol), .gnu.hash, .hash,
    // .dynstr, .rela.dyn at 0x210, .rodata, .eh_frame_hdr and .eh_frame, 0x8BC bytes.
    let mut r_seg = vec![0u8; 0x8BC];
    let rela_at = 0x210usize;
    // 38 slots in the RW segment's .data, after .dynamic (0x4000..0x40E0): a run of 30,
    // then 8 with gaps, the last one ending at 0x42D8.
    let isolated = [
        0x4200, 0x4210, 0x4238, 0x4250, 0x4260, 0x4288, 0x42A0, 0x42D0,
    ];
    let slots = (0..30).map(|i| 0x40E0 + 8 * i).chain(isolated);
    let relocations: Vec<(u64, u64)> = slots
        .enumerate()
        .map(|(i, slot)| (slot, 0x200 + 16 * i as u64))
        .collect();
    assert_eq!(relocations.len(), 38);
    assert_eq!(relocations.last().unwrap().0 + 8, 0x42D8);
    for (i, (slot, addend)) in relocations.iter().enumerate() {
        let at = rela_at + 24 * i;
        r_seg[at..at + 8].copy_from_slice(&slot.to_le_bytes());
        r_seg[at + 8..at + 16].copy_from_slice(&RELATIVE.to_le_bytes());
        r_seg[at + 16..at + 24].copy_from_slice(&addend.to_le_bytes());
    }
    raw.put(0, &r_seg);
    raw.put(0x1000, &text(0x2253));
    // RW segment: .dynamic first, then .data (zeros: lld leaves RELA slots zero).
    let mut rw = vec![0u8; 0x2E0];
    let dynamic: [(u64, u64); 14] = [
        (30, 0x8),
        (0x6FFF_FFFB, 0x0800_0001),
        (21, 0),
        (7, rela_at as u64),
        (8, 912),
        (9, 24),
        (0x6FFF_FFF9, 38),
        (6, 0x1C8),
        (11, 24),
        (5, 0x20C),
        (10, 1),
        (0x6FFF_FEF5, 0x1E0),
        (4, 0x1FC),
        (0, 0),
    ];
    for (i, (tag, value)) in dynamic.iter().enumerate() {
        rw[16 * i..16 * i + 8].copy_from_slice(&tag.to_le_bytes());
        rw[16 * i + 8..16 * i + 16].copy_from_slice(&value.to_le_bytes());
    }
    raw.put(0x4000, &rw);
    let load = |flags, vaddr, filesz, memsz| Phdr {
        p_type: PT_LOAD,
        flags,
        offset: vaddr,
        vaddr,
        filesz,
        memsz,
        align: 0x1000,
    };
    raw.phdrs = vec![
        Phdr {
            p_type: 6,
            flags: R,
            offset: 0x40,
            vaddr: 0x40,
            filesz: 7 * 56,
            memsz: 7 * 56,
            align: 8,
        },
        load(R, 0, 0x8BC, 0x8BC),
        load(RX, 0x1000, 0x2253, 0x2253),
        load(RW, 0x4000, 0x2E0, 0x2E1),
        Phdr {
            p_type: PT_DYNAMIC,
            flags: RW,
            offset: 0x4000,
            vaddr: 0x4000,
            filesz: 0xE0,
            memsz: 0xE0,
            align: 8,
        },
        Phdr {
            p_type: 0x6474_E550,
            flags: R,
            offset: 0x800,
            vaddr: 0x800,
            filesz: 0x24,
            memsz: 0x24,
            align: 4,
        },
        Phdr::other(PT_GNU_STACK, RW),
    ];
    (raw.bytes(), relocations)
}

#[test]
fn framekernel_shaped_elf_converts() {
    let (elf, relocations) = framekernel_shape();
    let p = pack(&elf, &options()).unwrap();
    let k = inspect(&p.bytes);
    assert_eq!((p.sections, p.segments, p.relocations), (6, 3, 38));
    assert_eq!(
        kinds(&k.image),
        [
            Kind::Manifest,
            Kind::Segment,
            Kind::Segment,
            Kind::Segment,
            Kind::Relocs,
            Kind::Merkle
        ]
    );
    let placed: Vec<(u64, u64, u64)> = k
        .segments
        .iter()
        .map(|s| (s.entry.mem_offset, s.entry.mem_size, s.entry.content_size))
        .collect();
    assert_eq!(
        placed,
        [
            (0, 0x1000, 0x8BC),
            (0x1000, 0x3000, 0x2253),
            (0x4000, 0x1000, 0x2E0)
        ]
    );
    // The 0x2253-byte text has 3 blocks, so one hash block in section 5.
    assert_eq!(k.segments[1].entry.merkle_index, 5);
    assert_eq!(k.manifest.entry, 0x14F0);
    // All 38 addends were written into the RW content.
    let slots: Vec<u64> = relocations.iter().map(|r| r.0).collect();
    assert_eq!(k.relocations.as_deref(), Some(&slots[..]));
    for (slot, addend) in &relocations {
        assert_eq!(u64_at(k.segments[2].stored, slot - 0x4000), *addend);
    }
    // The R segment, including .eh_frame_hdr, is carried unchanged.
    assert_eq!(k.segments[0].stored, &elf[..0x8BC]);
}

/// The memory placement and RELRO bit of each SEGMENT.
fn placements(segments: &[Section<'_>]) -> Vec<(u64, u64, u64, bool)> {
    segments
        .iter()
        .map(|s| {
            let e = &s.entry;
            (
                e.mem_offset,
                e.mem_size,
                e.content_size,
                e.flags.has(Flags::RELRO),
            )
        })
        .collect()
}

/// ADR-0001 default 9: `PT_GNU_RELRO` covering all of an R+W `PT_LOAD`, the shape `rust-lld`
/// writes: that segment is flagged, nothing is split, and the data after it stays plain RW.
#[test]
fn relro_flags_the_load_it_covers() {
    let mut elf = TestElf::kernel();
    elf.loads[2] = Load::new(RW, 0x3000, vec![0x11; 0x20]).mem(0x1000);
    elf.loads
        .push(Load::new(RW, 0x4000, vec![0x22; 0x40]).mem(0x2000));
    elf.extra.push(Phdr::relro(0x3000, 0x1000));
    elf.rela = vec![(0x3008, RELATIVE, 0x2000), (0x4010, RELATIVE, 0x2010)];
    let packed = pack(&elf.bytes(), &options()).unwrap();
    let k = inspect(&packed.bytes);
    assert_eq!(
        placements(&k.segments)[2..4],
        [(0x3000, 0x1000, 0x20, true), (0x4000, 0x2000, 0x40, false)]
    );
    let expected = Flags::REQUIRED | Flags::MAPPABLE | Flags::READ | Flags::WRITE;
    assert_eq!(k.segments[2].entry.flags.0, expected | Flags::RELRO);
    assert_eq!(k.segments[3].entry.flags.0, expected);
    // Relocations land in both, and the addend was written into the RELRO slot.
    assert_eq!(k.relocations, Some(vec![0x3008, 0x4010]));
    assert_eq!(k.segments[2].stored[8..16], 0x2000u64.to_le_bytes());
}

/// A linker that keeps the relocated data and the rest in one R+W `PT_LOAD`: the packer splits it
/// at the boundary, on a page, and the bytes on either side land in the right section.
#[test]
fn relro_splits_the_load_at_its_page_boundary() {
    let data: Vec<u8> = (0..0x1800u32).map(|i| (i % 251) as u8).collect();
    let mut elf = TestElf::kernel();
    elf.loads[2] = Load::new(RW, 0x3000, data.clone()).mem(0x3000);
    elf.extra.push(Phdr::relro(0x3000, 0x1000));
    elf.rela = vec![(0x3ff8, RELATIVE, 0x2000), (0x4000, RELATIVE, 0x2010)];
    let packed = pack(&elf.bytes(), &options()).unwrap();
    let k = inspect(&packed.bytes);
    assert_eq!(
        placements(&k.segments)[2..4],
        [
            (0x3000, 0x1000, 0x1000, true),
            (0x4000, 0x2000, 0x800, false)
        ]
    );
    assert_eq!(&k.segments[2].stored[..0xFF8], &data[..0xFF8]);
    assert_eq!(k.segments[2].stored[0xFF8..], 0x2000u64.to_le_bytes());
    assert_eq!(k.segments[3].stored[..8], 0x2010u64.to_le_bytes());
    assert_eq!(&k.segments[3].stored[8..], &data[0x1008..]);
    assert_eq!(k.relocations, Some(vec![0x3ff8, 0x4000]));

    // RELRO covers every file byte: the rest of the PT_LOAD is bss, a segment with no content.
    let mut elf = TestElf::kernel();
    elf.loads[2] = Load::new(RW, 0x3000, vec![0x33; 0x400]).mem(0x3000);
    elf.extra.push(Phdr::relro(0x3000, 0x2000));
    let packed = pack(&elf.bytes(), &options()).unwrap();
    let k = inspect(&packed.bytes);
    assert_eq!(
        placements(&k.segments)[2..4],
        [(0x3000, 0x2000, 0x400, true), (0x5000, 0x1000, 0, false)]
    );
}

/// Without `PT_GNU_RELRO` nothing is flagged, which is what `-z norelro` output looks like.
#[test]
fn no_relro_header_means_no_relro_segment() {
    let packed = pack(&TestElf::kernel().bytes(), &options()).unwrap();
    let k = inspect(&packed.bytes);
    assert!(placements(&k.segments).iter().all(|&(.., relro)| !relro));
}
