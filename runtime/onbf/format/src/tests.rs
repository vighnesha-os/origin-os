//! Container tests: a canonical test-only image builder, the malicious corpus (ONBF §13
//! item 4, container cases), and exhaustive truncation, padding and bit-flip sweeps.
//!
//! The builder is test scaffolding, not `onbf-pack`. `fake_sha512` is a non-cryptographic
//! stand-in that only exercises the digest plumbing; real SHA-512 comes from
//! `origin-crypto`.

use crate::*;

const MANIFEST: u32 = 0x0001;
const STRINGS: u32 = 0x0002;
const SEGMENT: u32 = 0x0010;
const IMPORTS: u32 = 0x0012;
const MERKLE: u32 = 0x00F0;
const REQ: u32 = Flags::REQUIRED;
const MAP: u32 = Flags::MAPPABLE;
const R: u32 = Flags::READ;
const W: u32 = Flags::WRITE;
const X: u32 = Flags::EXECUTE;

#[derive(Clone)]
struct Sec {
    kind: u32,
    flags: u32,
    stored: Vec<u8>,
    content_size: Option<u64>,
    mem_offset: u64,
    mem_size: Option<u64>,
    compression: u8,
    integrity: u8,
    merkle_index: u32,
    digest: [u8; 32],
}

impl Sec {
    fn manifest() -> Self {
        Self {
            kind: MANIFEST,
            flags: REQ,
            stored: vec![0xA0; 16],
            content_size: None,
            mem_offset: 0,
            mem_size: None,
            compression: 0,
            integrity: 0,
            merkle_index: NO_MERKLE,
            digest: [0x11; 32],
        }
    }

    fn segment(perms: u32, len: usize, mem_offset: u64, mem_size: u64, merkle_index: u32) -> Self {
        Self {
            kind: SEGMENT,
            flags: MAP | perms,
            stored: vec![0xCC; len],
            mem_offset,
            mem_size: Some(mem_size),
            integrity: 1,
            merkle_index,
            digest: [0x22; 32],
            ..Self::manifest()
        }
    }

    fn merkle(blocks: usize) -> Self {
        Self {
            kind: MERKLE,
            flags: 0,
            stored: vec![0x33; blocks * 4096],
            integrity: 2,
            digest: [0; 32],
            ..Self::manifest()
        }
    }

    fn plain(kind: u32, len: usize) -> Self {
        Self {
            kind,
            flags: 0,
            stored: vec![0x44; len],
            ..Self::manifest()
        }
    }
}

#[derive(Clone)]
struct Builder {
    profile: u16,
    sections: Vec<Sec>,
    signature: Option<Vec<u8>>,
}

fn align(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

/// Non-cryptographic stand-in for SHA-512 (FNV-1a lanes).
fn fake_sha512(parts: &[&[u8]; 3]) -> [u8; 64] {
    let mut out = [0u8; 64];
    for (lane, chunk) in out.chunks_mut(8).enumerate() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (lane as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for p in parts {
            for &b in *p {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
            h = (h ^ 0xFF).wrapping_mul(0x0100_0000_01b3);
        }
        chunk.copy_from_slice(&h.to_le_bytes());
    }
    out
}

impl Builder {
    /// A KRN image: manifest, an RX segment large enough to need one hash block, its
    /// MERKLE section, and a small RW segment that needs none. Signed.
    ///
    /// Layout: table ends at 640; manifest 4096..4112; text 8192..13192; MERKLE
    /// 13192..17288; data 20480..20580; signature 20584..20648.
    fn kernel() -> Self {
        Self {
            profile: Profile::Krn as u16,
            sections: vec![
                Sec::manifest(),
                Sec::segment(R | X, 5000, 0, 8192, 2),
                Sec::merkle(1),
                Sec::segment(R | W, 100, 8192, 4096, NO_MERKLE),
            ],
            signature: Some(vec![0x5A; 64]),
        }
    }

    fn minimal(profile: u16) -> Self {
        Self {
            profile,
            sections: vec![Sec::manifest()],
            signature: Some(vec![0x5A; 64]),
        }
    }

    fn unsigned(mut self) -> Self {
        self.signature = None;
        self
    }

    fn build(&self) -> Vec<u8> {
        let n = self.sections.len();
        let table_end = 128 + 128 * n;
        let mut pos = align(table_end, 4096);
        let mut offsets = Vec::new();
        for s in &self.sections {
            let off = align(pos, if s.flags & MAP != 0 { 4096 } else { 8 });
            offsets.push(off);
            pos = off + s.stored.len();
        }
        let sig_off = self.signature.as_ref().map(|_| align(pos, 8));
        let sig_len = self.signature.as_ref().map_or(0, Vec::len);
        let total = sig_off.map_or(pos, |o| o + sig_len);
        let mut out = vec![0u8; total];

        out[0..8].copy_from_slice(&MAGIC);
        put16(&mut out, 8, 1);
        put16(&mut out, 12, self.profile);
        put16(&mut out, 14, 128);
        put32(&mut out, 20, n as u32);
        put64(&mut out, 24, 128);
        put64(&mut out, 32, sig_off.unwrap_or(0) as u64);
        put64(&mut out, 40, sig_len as u64);
        put16(&mut out, 112, 1);

        for (i, (s, &off)) in self.sections.iter().zip(&offsets).enumerate() {
            let e = entry(i);
            let content = s.content_size.unwrap_or(s.stored.len() as u64);
            put32(&mut out, e, s.kind);
            put32(&mut out, e + 4, s.flags);
            put64(&mut out, e + 8, off as u64);
            put64(&mut out, e + 16, s.stored.len() as u64);
            put64(&mut out, e + 24, content);
            put64(&mut out, e + 32, s.mem_offset);
            put64(&mut out, e + 40, s.mem_size.unwrap_or(content));
            out[e + 48] = s.compression;
            out[e + 49] = s.integrity;
            put32(&mut out, e + 52, s.merkle_index);
            out[e + 64..e + 96].copy_from_slice(&s.digest);
            out[off..off + s.stored.len()].copy_from_slice(&s.stored);
        }
        if let (Some(o), Some(sig)) = (sig_off, &self.signature) {
            out[o..o + sig.len()].copy_from_slice(sig);
        }
        let digest = fake_sha512(&[&out[0..32], &out[112..128], &out[128..table_end]]);
        out[48..112].copy_from_slice(&digest);
        out
    }
}

fn entry(i: usize) -> usize {
    128 + 128 * i
}
fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

fn strict() -> ParseOptions {
    ParseOptions::default()
}
fn dev() -> ParseOptions {
    ParseOptions {
        developer_mode: true,
        ..ParseOptions::default()
    }
}
fn nested() -> ParseOptions {
    ParseOptions {
        nested_in_verified_parent: true,
        ..ParseOptions::default()
    }
}
fn sec(index: u16, error: SectionError) -> Error {
    Error::Section { index, error }
}
fn parse(img: &[u8]) -> Result<Image<'_>, Error> {
    Image::parse(img, strict())
}

// ------------------------------------------------------------------ valid images

#[test]
fn kernel_image_parses_with_canonical_layout() {
    let img = Builder::kernel().build();
    assert_eq!(img.len(), 20648);
    let image = parse(&img).expect("valid kernel image");
    let h = image.header();
    assert_eq!(
        (h.format_major, h.format_minor, h.profile, h.section_count),
        (1, 0, Profile::Krn, 4)
    );
    let offsets: Vec<u64> = image.sections().map(|s| s.entry.file_offset).collect();
    assert_eq!(offsets, vec![4096, 8192, 13192, 20480]);
    let kinds: Vec<Kind> = image.sections().map(|s| s.entry.kind()).collect();
    assert_eq!(
        kinds,
        vec![Kind::Manifest, Kind::Segment, Kind::Merkle, Kind::Segment]
    );
    assert_eq!(image.section(1).map(|s| s.stored.len()), Some(5000));
    assert!(image.section(4).is_none());
    assert_eq!(image.manifest().map(|s| s.index), Some(0));
    assert_eq!(image.signature_block().map(<[u8]>::len), Some(64));
    assert_eq!(
        image.section(1).map(|s| s.entry.integrity),
        Some(Integrity::Merkle)
    );
}

#[test]
fn content_digest_covers_header_and_table_but_not_signature_location() {
    let img = Builder::kernel().build();
    let image = parse(&img).unwrap();
    let input = image.content_digest_input();
    assert_eq!(input.map(<[u8]>::len), [32, 16, 512]);
    assert_eq!(image.verify_content_digest(fake_sha512), Ok(()));

    // A covered field that still parses: format_minor.
    let mut minor = img.clone();
    put16(&mut minor, 10, 1);
    let image = parse(&minor).expect("a higher minor still parses (§4.2)");
    assert_eq!(
        image.verify_content_digest(fake_sha512),
        Err(Error::ContentDigestMismatch)
    );

    // An entry digest (inside the table) and the stored digest itself.
    for at in [entry(1) + 64, 48] {
        let mut t = img.clone();
        t[at] ^= 1;
        let image = parse(&t).unwrap();
        assert_eq!(
            image.verify_content_digest(fake_sha512),
            Err(Error::ContentDigestMismatch),
            "byte {at}"
        );
    }
}

#[test]
fn every_profile_accepts_a_minimal_image() {
    for profile in 1..=8u16 {
        let img = Builder::minimal(profile).build();
        let image = parse(&img).unwrap_or_else(|e| panic!("profile {profile}: {e:?}"));
        assert_eq!(image.header().profile as u16, profile);
    }
}

#[test]
fn unsigned_images_need_developer_mode_or_a_verified_parent() {
    let img = Builder::kernel().unsigned().build();
    assert_eq!(
        Image::parse(&img, strict()).err(),
        Some(Error::UnsignedNotAllowed)
    );
    assert!(Image::parse(&img, dev()).is_ok());
    assert!(Image::parse(&img, nested()).is_ok());
    assert!(
        Image::parse(&img, dev())
            .unwrap()
            .signature_block()
            .is_none()
    );
}

#[test]
fn unknown_optional_kind_is_skippable_but_unknown_required_kind_is_not() {
    let mut b = Builder::kernel();
    b.sections.push(Sec::plain(0x7777, 10));
    let img = b.build();
    let image = parse(&img).expect("unknown kind without REQUIRED is allowed (§4.4)");
    assert_eq!(
        image.section(4).map(|s| s.entry.kind()),
        Some(Kind::Unknown(0x7777))
    );

    b.sections[4].flags = REQ;
    assert_eq!(
        parse(&b.build()).err(),
        Some(sec(4, SectionError::UnknownRequiredKind(0x7777)))
    );
}

#[test]
fn bss_segment_and_nested_parent_coverage() {
    let mut b = Builder::kernel();
    b.sections
        .push(Sec::segment(R | W, 0, 12288, 4096, NO_MERKLE));
    assert!(
        parse(&b.build()).is_ok(),
        "empty segment with mem_size > 0 (bss)"
    );

    let mut b = Builder::kernel().unsigned();
    b.sections[3].integrity = 3;
    b.sections[3].digest = [0; 32];
    let img = b.build();
    assert!(Image::parse(&img, nested()).is_ok());
    assert_eq!(
        Image::parse(&img, dev()).err(),
        Some(sec(3, SectionError::IntegrityNotAllowed(3)))
    );
}

// ------------------------------------------------------------------ malicious corpus

type Case = (&'static str, Vec<u8>, ParseOptions, Error);

fn mutated(f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut img = Builder::kernel().build();
    f(&mut img);
    img
}

fn built(f: impl FnOnce(&mut Builder)) -> Vec<u8> {
    let mut b = Builder::kernel();
    f(&mut b);
    b.build()
}

fn header_cases() -> Vec<Case> {
    let s = strict();
    vec![
        ("bad magic", mutated(|i| i[1] = b'X'), s, Error::BadMagic),
        (
            "CR/LF rewritten in magic",
            mutated(|i| i[5] = 0x0A),
            s,
            Error::BadMagic,
        ),
        (
            "format major 2",
            mutated(|i| put16(i, 8, 2)),
            s,
            Error::UnsupportedFormatMajor(2),
        ),
        (
            "profile 0",
            mutated(|i| put16(i, 12, 0)),
            s,
            Error::UnknownProfile(0),
        ),
        (
            "profile 9",
            mutated(|i| put16(i, 12, 9)),
            s,
            Error::UnknownProfile(9),
        ),
        (
            "header size 64",
            mutated(|i| put16(i, 14, 64)),
            s,
            Error::BadHeaderSize(64),
        ),
        (
            "header flags",
            mutated(|i| put32(i, 16, 1)),
            s,
            Error::HeaderFlags(1),
        ),
        (
            "zero sections",
            mutated(|i| put32(i, 20, 0)),
            s,
            Error::SectionCount(0),
        ),
        (
            "257 sections",
            mutated(|i| put32(i, 20, 257)),
            s,
            Error::SectionCount(257),
        ),
        (
            "table offset 256",
            mutated(|i| put64(i, 24, 256)),
            s,
            Error::SectionTableOffset(256),
        ),
        (
            "arch 0",
            mutated(|i| put16(i, 112, 0)),
            s,
            Error::UnsupportedArch(0),
        ),
        (
            "arch 3 (no 32-bit or x32 value exists)",
            mutated(|i| put16(i, 112, 3)),
            s,
            Error::UnsupportedArch(3),
        ),
        (
            "header reserved byte",
            mutated(|i| i[120] = 1),
            s,
            Error::HeaderReserved,
        ),
        (
            "signature block over 1 MiB",
            mutated(|i| put64(i, 40, (1 << 20) + 1)),
            s,
            Error::SignatureBlockSize((1 << 20) + 1),
        ),
        (
            "signature offset with zero size",
            mutated(|i| put64(i, 40, 0)),
            s,
            Error::SignatureBlockSize(0),
        ),
        (
            "signature size without offset",
            {
                let mut i = Builder::kernel().unsigned().build();
                put64(&mut i, 40, 8);
                i
            },
            dev(),
            Error::SignatureBlockSize(8),
        ),
        (
            "signature block misplaced",
            mutated(|i| put64(i, 32, 20592)),
            s,
            Error::SignatureBlockMisplaced {
                expected: 20584,
                found: 20592,
            },
        ),
        (
            "trailing byte after signature",
            mutated(|i| i.push(0)),
            s,
            Error::TrailingData,
        ),
        (
            "truncated signature",
            mutated(|i| _ = i.pop()),
            s,
            Error::Truncated,
        ),
        (
            "trailing byte after unsigned image",
            {
                let mut i = Builder::kernel().unsigned().build();
                i.push(0);
                i
            },
            dev(),
            Error::TrailingData,
        ),
        (
            "truncated header",
            mutated(|i| i.truncate(100)),
            s,
            Error::Truncated,
        ),
        (
            "truncated table",
            mutated(|i| i.truncate(300)),
            s,
            Error::Truncated,
        ),
        (
            "table padding not zero",
            mutated(|i| i[640] = 1),
            s,
            Error::PaddingNotZero(640),
        ),
        (
            "padding between sections not zero",
            mutated(|i| i[4112] = 1),
            s,
            Error::PaddingNotZero(4112),
        ),
        (
            "padding before signature not zero",
            mutated(|i| i[20581] = 1),
            s,
            Error::PaddingNotZero(20581),
        ),
    ]
}

fn section_cases() -> Vec<Case> {
    use SectionError as E;
    let s = strict();
    let e0 = entry(0);
    let e1 = entry(1);
    let e2 = entry(2);
    let e3 = entry(3);
    vec![
        (
            "unknown flag bit",
            mutated(|i| put32(i, e1 + 4, MAP | R | X | 0x40)),
            s,
            sec(1, E::UnknownFlags(0x40)),
        ),
        (
            "W+X segment",
            mutated(|i| put32(i, e1 + 4, MAP | R | W | X)),
            s,
            sec(1, E::WriteAndExecute),
        ),
        (
            "permissions on the manifest",
            mutated(|i| put32(i, e0 + 4, REQ | R)),
            s,
            sec(0, E::PermissionsOnNonSegment),
        ),
        (
            "segment not mappable",
            mutated(|i| put32(i, e1 + 4, R | X)),
            s,
            sec(1, E::SegmentNotMappable),
        ),
        (
            "unknown required kind",
            mutated(|i| {
                put32(i, e2, 0x7777);
                put32(i, e2 + 4, REQ);
            }),
            s,
            sec(2, E::UnknownRequiredKind(0x7777)),
        ),
        (
            "IMPORTS in a KRN image",
            mutated(|i| put32(i, e2, IMPORTS)),
            s,
            sec(2, E::KindNotAllowedInProfile(IMPORTS)),
        ),
        (
            "compression 2",
            mutated(|i| i[e0 + 48] = 2),
            s,
            sec(0, E::UnknownCompression(2)),
        ),
        (
            "compressed mappable segment",
            mutated(|i| i[e1 + 48] = 1),
            s,
            sec(1, E::CompressionNotAllowed),
        ),
        (
            "compressed MERKLE section",
            mutated(|i| i[e2 + 48] = 1),
            s,
            sec(2, E::CompressionNotAllowed),
        ),
        (
            "content over 1 TiB",
            mutated(|i| {
                put64(i, e0 + 16, (1 << 40) + 1);
                put64(i, e0 + 24, (1 << 40) + 1);
            }),
            s,
            sec(0, E::TooLarge),
        ),
        (
            "stored_size near u64::MAX",
            mutated(|i| put64(i, e3 + 16, u64::MAX - 8)),
            s,
            sec(3, E::TooLarge),
        ),
        (
            "uncompressed size mismatch",
            mutated(|i| put64(i, e0 + 24, 17)),
            s,
            sec(0, E::SizeMismatch),
        ),
        (
            "integrity 4",
            mutated(|i| i[e0 + 49] = 4),
            s,
            sec(0, E::UnknownIntegrity(4)),
        ),
        (
            "Merkle integrity on the manifest",
            mutated(|i| i[e0 + 49] = 1),
            s,
            sec(0, E::IntegrityNotAllowed(1)),
        ),
        (
            "SHA-256 integrity on a mapped segment",
            mutated(|i| i[e3 + 49] = 0),
            s,
            sec(3, E::IntegrityNotAllowed(0)),
        ),
        (
            "parent coverage in a top-level image",
            mutated(|i| {
                i[e3 + 49] = 3;
                i[e3 + 64..e3 + 96].fill(0);
            }),
            s,
            sec(3, E::IntegrityNotAllowed(3)),
        ),
        (
            "MERKLE section not using integrity 2",
            mutated(|i| i[e2 + 49] = 0),
            s,
            sec(2, E::IntegrityNotAllowed(0)),
        ),
        (
            "hash-block integrity outside MERKLE",
            mutated(|i| i[e0 + 49] = 2),
            s,
            sec(0, E::IntegrityNotAllowed(2)),
        ),
        (
            "merkle_index on a SHA-256 section",
            mutated(|i| put32(i, e0 + 52, 2)),
            s,
            sec(0, E::MerkleIndex(2)),
        ),
        (
            "digest on a MERKLE section",
            mutated(|i| i[e2 + 64] = 1),
            s,
            sec(2, E::DigestNotZero),
        ),
        (
            "manifest without REQUIRED",
            mutated(|i| put32(i, e0 + 4, 0)),
            s,
            sec(0, E::ManifestRules),
        ),
        (
            "manifest over 1 MiB (compressed)",
            mutated(|i| {
                i[e0 + 48] = 1;
                put64(i, e0 + 24, 2 << 20);
                put64(i, e0 + 40, 2 << 20);
            }),
            s,
            sec(0, E::ManifestRules),
        ),
        (
            "segment RVA not page aligned",
            mutated(|i| put64(i, e3 + 32, 8193)),
            s,
            sec(3, E::MemoryLayout),
        ),
        (
            "segment smaller in memory than content",
            mutated(|i| put64(i, e1 + 40, 4096)),
            s,
            sec(1, E::MemoryLayout),
        ),
        (
            "segment memory range overflows",
            mutated(|i| put64(i, e3 + 32, !4095)),
            s,
            sec(3, E::MemoryLayout),
        ),
        (
            "non-segment with an RVA",
            mutated(|i| put64(i, e0 + 32, 4096)),
            s,
            sec(0, E::MemoryLayout),
        ),
        (
            "non-segment mem_size != content_size",
            mutated(|i| put64(i, e2 + 40, 0)),
            s,
            sec(2, E::MemoryLayout),
        ),
        (
            "entry reserved byte 50",
            mutated(|i| i[e0 + 50] = 1),
            s,
            sec(0, E::Reserved),
        ),
        (
            "entry reserved byte 56",
            mutated(|i| i[e0 + 56] = 1),
            s,
            sec(0, E::Reserved),
        ),
        (
            "entry reserved byte 96",
            mutated(|i| i[e0 + 127] = 1),
            s,
            sec(0, E::Reserved),
        ),
        (
            "section after a gap",
            mutated(|i| put64(i, e1 + 8, 12288)),
            s,
            sec(
                1,
                E::Misplaced {
                    expected: 8192,
                    found: 12288,
                },
            ),
        ),
        (
            "overlapping sections",
            mutated(|i| put64(i, e2 + 8, 13184)),
            s,
            sec(
                2,
                E::Misplaced {
                    expected: 13192,
                    found: 13184,
                },
            ),
        ),
        (
            "misaligned mappable section",
            mutated(|i| put64(i, e3 + 8, 20484)),
            s,
            sec(
                3,
                E::Misplaced {
                    expected: 20480,
                    found: 20484,
                },
            ),
        ),
        (
            "file_offset u64::MAX",
            mutated(|i| put64(i, e3 + 8, u64::MAX)),
            s,
            sec(
                3,
                E::Misplaced {
                    expected: 20480,
                    found: u64::MAX,
                },
            ),
        ),
        (
            "section past end of file",
            mutated(|i| {
                put64(i, e3 + 16, 1 << 30);
                put64(i, e3 + 24, 1 << 30);
                put64(i, e3 + 40, 1 << 30);
            }),
            s,
            sec(3, E::OutOfBounds),
        ),
        (
            "file cut inside a section",
            mutated(|i| i.truncate(9000)),
            s,
            sec(1, E::OutOfBounds),
        ),
        (
            "merkle_index out of range",
            mutated(|i| put32(i, e1 + 52, 9)),
            s,
            sec(1, E::MerkleIndex(9)),
        ),
        (
            "merkle_index at a non-MERKLE section",
            mutated(|i| put32(i, e1 + 52, 0)),
            s,
            sec(1, E::MerkleIndex(0)),
        ),
        (
            "merkle_index although the tree has no hash blocks",
            mutated(|i| put32(i, e3 + 52, 2)),
            s,
            sec(3, E::MerkleIndex(2)),
        ),
        (
            "MERKLE section of the wrong size",
            built(|b| b.sections[2] = Sec::merkle(2)),
            s,
            sec(
                2,
                E::MerkleSize {
                    expected: 4096,
                    found: 8192,
                },
            ),
        ),
        (
            "unreferenced MERKLE section",
            built(|b| b.sections.push(Sec::merkle(1))),
            s,
            sec(4, E::MerkleReferences(0)),
        ),
        (
            "MERKLE section shared by two segments",
            built(|b| b.sections.push(Sec::segment(R, 5000, 16384, 8192, 2))),
            s,
            sec(2, E::MerkleReferences(2)),
        ),
        (
            "two manifests",
            built(|b| b.sections.push(Sec::manifest())),
            s,
            Error::ManifestCount,
        ),
        (
            "no manifest",
            {
                let mut b = Builder::minimal(Profile::Exe as u16);
                b.sections[0] = Sec::plain(STRINGS, 8);
                b.build()
            },
            s,
            Error::ManifestCount,
        ),
    ]
}

#[test]
fn malicious_corpus_is_rejected_with_the_right_error() {
    let cases: Vec<Case> = header_cases().into_iter().chain(section_cases()).collect();
    let mut failures = Vec::new();
    for (name, img, opts, want) in &cases {
        let got = Image::parse(img, *opts).err();
        if got != Some(*want) {
            failures.push(format!("{name}: want {want:?}, got {got:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    assert!(cases.len() >= 65, "corpus has {} cases", cases.len());
}

// ------------------------------------------------------------------ sweeps

#[test]
fn every_truncation_is_rejected() {
    let img = Builder::kernel().build();
    for cut in 0..img.len() {
        assert!(parse(&img[..cut]).is_err(), "accepted a file cut at {cut}");
    }
}

#[test]
fn every_padding_byte_is_checked() {
    let img = Builder::kernel().build();
    let padding = [640..4096, 4112..8192, 17288..20480, 20580..20584];
    let mut checked = 0;
    for range in padding {
        for at in range {
            let mut t = img.clone();
            t[at] = 0x01;
            assert_eq!(
                parse(&t).err(),
                Some(Error::PaddingNotZero(at as u64)),
                "byte {at}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 3456 + 4080 + 3192 + 4);
}

/// Flips every bit of the header and section table. A flipped image must be rejected
/// unless the bit is in a field that legitimately has other valid values. Those fields
/// are still covered by `content_digest`, so they cannot change undetected.
#[test]
fn bit_flips_in_header_and_table_are_rejected_outside_free_fields() {
    let img = Builder::kernel().build();
    // Fields with more than one valid value for this image, all covered by content_digest:
    // format_minor (higher minors are readable), the digest itself, and `profile`, because
    // this section set is also a valid LIB image. The signed statement names the profile
    // too (§7.1), so a profile swap fails signature verification.
    let free = |at: usize| -> bool {
        let header_free = (10..14).contains(&at) || (48..112).contains(&at);
        if at < 128 {
            return header_free;
        }
        let (i, off) = ((at - 128) / 128, (at - 128) % 128);
        let digest = (64..96).contains(&off) && i != 2; // MERKLE digest must stay zero
        let flags = (4..8).contains(&off) && i != 0; // segment perms; REQUIRED on optional kinds
        let memory = (32..48).contains(&off) && (i == 1 || i == 3); // segment placement
        let compression = off == 48 && i == 0; // a non-mapped manifest may be zstd
        digest || flags || memory || compression
    };
    let mut accepted_free = 0;
    let mut unexpected = Vec::new();
    for at in 0..640 {
        for bit in 0..8 {
            let mut t = img.clone();
            t[at] ^= 1 << bit;
            match parse(&t) {
                Err(_) => {}
                Ok(_) if free(at) => accepted_free += 1,
                Ok(image) => unexpected.push(format!(
                    "byte {at} bit {bit} (profile now {:?})",
                    image.header().profile
                )),
            }
        }
    }
    assert!(
        unexpected.is_empty(),
        "accepted outside free fields:\n{}",
        unexpected.join("\n")
    );
    assert!(accepted_free > 0);
}
