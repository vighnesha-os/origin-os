//! ONBF images (docs/spec/ONBF.md §4, §6.5, §7.1) written from the fuzzer's choices and
//! sealed, and every reader of them run: the container parser, the content digest, the
//! section digests and Merkle trees, the KRN manifest, the signature block, and the UEFI
//! loader's verification and placement (ADR-0002).
//!
//! [`image`] writes a KRN image (or, now and then, another profile) laid out as §4.1 requires,
//! with every digest sealed, then perhaps flips a few bytes and perhaps seals again. [`seal`]
//! recomputes the section digests, the Merkle hash blocks and roots, and the content digest of
//! any bytes, as far as their table can be read, so that byte-level mutations of a valid image
//! still reach the checks behind the digests.

use arbitrary::{Result, Unstructured};
use onbf_format::{Flags, Image, Integrity, Kind, KrnManifest, ParseOptions, Section};
use onbf_merkle::{NoCache, Shape};
use origin_boot_info::{KernelSegment, MAX_KERNEL_SEGMENTS};
use origin_boot_uefi::{kernel, place, trust::STORE};
use origin_crypto::{Sha256, Sha512};

use crate::ocbor::{KrnFields, krn_manifest};

/// `"\x89ONBF\r\n\x1a"` (§4.2).
const MAGIC: [u8; 8] = [0x89, b'O', b'N', b'B', b'F', 0x0D, 0x0A, 0x1A];
const HEADER: usize = 128;
const ENTRY: usize = 128;
const PAGE: usize = 4096;
const PAGE64: u64 = 4096;
/// `merkle_index` when there is no MERKLE section (§4.3).
const NO_MERKLE: u32 = u32::MAX;
/// The KRN profile's ID (§1).
const KRN: u16 = 6;
/// Section kinds a KRN image may hold (§4.4).
const MANIFEST: u32 = 0x0001;
const SEGMENT: u32 = 0x0010;
const RELOCS: u32 = 0x0011;
const EXPORTS: u32 = 0x0013;
const UNWIND: u32 = 0x0015;
const MERKLE: u32 = 0x00F0;
const DEBUG_LINK: u32 = 0x00F1;
/// Section flags (§4.3).
const REQUIRED: u32 = 1 << 0;
const MAPPABLE: u32 = 1 << 1;
const READ: u32 = 1 << 2;
const WRITE: u32 = 1 << 3;
const EXECUTE: u32 = 1 << 4;
const RELRO: u32 = 1 << 5;
/// Errata G4: segments end within a 2 GiB image span.
const MAX_SPAN: u64 = 2 << 30;
/// Spans larger than this are measured but not placed, so one run stays small.
const MAX_PLACE: u64 = 16 << 20;
/// Where the loader places the kernel (any value: placement writes it into relocations).
const BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// One section as the writer plans it.
struct Plan {
    kind: u32,
    flags: u32,
    stored: Vec<u8>,
    mem_offset: u64,
    mem_size: u64,
    integrity: u8,
    merkle_index: u32,
}

impl Plan {
    /// A section that is not mapped: no memory placement, `mem_size` its length (§4.3).
    fn plain(kind: u32, stored: Vec<u8>, integrity: u8) -> Self {
        Self {
            kind,
            flags: REQUIRED,
            mem_offset: 0,
            mem_size: n64(stored.len()),
            stored,
            integrity,
            merkle_index: NO_MERKLE,
        }
    }
}

fn n64(v: usize) -> u64 {
    u64::try_from(v).unwrap_or(u64::MAX)
}

fn round_up(v: usize, align: usize) -> usize {
    v.div_ceil(align).saturating_mul(align)
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn le64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn put(b: &mut [u8], at: usize, v: &[u8]) {
    if let Some(dst) = b.get_mut(at..at + v.len()) {
        dst.copy_from_slice(v);
    }
}

/// `len` bytes of content: up to 256 of the fuzzer's, repeated, or zeros if it has none left.
/// Content rarely matters to the parsers, so it costs little of the input.
fn content(u: &mut Unstructured<'_>, len: usize) -> Result<Vec<u8>> {
    let n = len.min(u.len()).min(256);
    let src = u.bytes(n)?;
    let mut v = vec![0u8; len];
    if !src.is_empty() {
        for (d, s) in v.iter_mut().zip(src.iter().cycle()) {
            *d = *s;
        }
    }
    Ok(v)
}

/// Writes an ONBF image from `u`'s choices: mostly a KRN image (§6.5) that the container
/// parser accepts and the loader verifies in developer mode, with every digest sealed; now and
/// then another profile, a signature block, a few flipped bytes, and the digests sealed again
/// or not.
pub fn image(u: &mut Unstructured<'_>) -> Result<Vec<u8>> {
    let profile: u16 = if u.ratio(7, 8)? {
        KRN
    } else {
        u.int_in_range(0..=9)?
    };
    let mut plans = vec![Plan::plain(MANIFEST, Vec::new(), 0)];
    let mut rva = 0u64;
    let mut entry = None;
    let mut writable: Vec<(u64, u64)> = Vec::new();
    for k in 0..u.int_in_range(1..=4)? {
        // The first segment is mostly code with content, so the entry point has somewhere to be.
        let perm = if k == 0 && u.ratio(7, 8)? {
            READ | EXECUTE
        } else {
            *u.choose(&[READ | EXECUTE, READ, READ | WRITE, READ | WRITE | RELRO])?
        };
        let least = usize::from(k == 0);
        let len = u.int_in_range(least..=9000usize)?;
        let stored = content(u, len)?;
        rva += PAGE64 * u.int_in_range(0..=2u64)?;
        let pages = n64(len.max(1).div_ceil(PAGE)) + u.int_in_range(0..=1u64)?;
        if perm == READ | EXECUTE && len > 0 && entry.is_none() {
            entry = Some(rva + u.int_in_range(0..=n64(len) - 1)?);
        }
        if perm & WRITE != 0 && len >= 8 {
            writable.push((rva, n64(len)));
        }
        plans.push(Plan {
            kind: SEGMENT,
            flags: REQUIRED | MAPPABLE | perm,
            stored,
            mem_offset: rva,
            mem_size: pages * PAGE64,
            integrity: 1,
            merkle_index: NO_MERKLE,
        });
        rva += pages * PAGE64;
    }
    if !writable.is_empty() && u.ratio(1, 2)? {
        plans.push(relocs(u, &writable)?);
    }
    for kind in [UNWIND, DEBUG_LINK, EXPORTS] {
        if u.ratio(1, 6)? {
            let len = u.int_in_range(1..=300)?;
            let stored = content(u, len)?;
            let integrity = if u.ratio(1, 4)? { 1 } else { 0 };
            plans.push(Plan::plain(kind, stored, integrity));
        }
    }

    // The manifest names the segments' digest and an entry point inside executable content.
    let mut build_id = Sha256::new();
    for p in plans.iter().filter(|p| p.kind == SEGMENT) {
        build_id.update(&p.stored);
    }
    let fields = KrnFields {
        build_id: build_id.finalize(),
        entry: match entry {
            Some(e) => e,
            None => u.arbitrary()?,
        },
        boot_info: if u.ratio(15, 16)? {
            origin_boot_info::VERSION
        } else {
            u.arbitrary()?
        },
        cpu_features: if u.ratio(15, 16)? { 0 } else { u.arbitrary()? },
        cet: (u.arbitrary()?, u.ratio(1, 16)?),
    };
    let manifest = krn_manifest(u, &fields)?;
    plans[0] = Plan::plain(MANIFEST, manifest, 0);
    if u.ratio(1, 4)? {
        let m = plans.remove(0);
        let at = u.int_in_range(0..=plans.len())?;
        plans.insert(at, m);
    }

    // A MERKLE section of hash blocks for each Merkle-protected section above one block (§4.6).
    for i in 0..plans.len() {
        if plans[i].integrity != 1 || plans[i].stored.len() <= PAGE {
            continue;
        }
        let Ok(shape) = Shape::new(n64(plans[i].stored.len())) else {
            continue;
        };
        let mut blocks = vec![0u8; usize::try_from(shape.hash_blocks_len()).unwrap_or(0)];
        if onbf_merkle::build(&plans[i].stored, &mut blocks).is_err() {
            continue;
        }
        plans[i].merkle_index = u32::try_from(plans.len()).unwrap_or(NO_MERKLE);
        plans.push(Plan::plain(MERKLE, blocks, 2));
    }

    let mut out = layout(profile, &plans);
    if u.ratio(1, 4)? {
        let at = round_up(out.len(), 8);
        out.resize(at, 0);
        let digest: [u8; 64] = out
            .get(48..112)
            .and_then(|d| d.try_into().ok())
            .unwrap_or([0; 64]);
        let block = crate::signature::block(u, &digest, u64::from(profile))?;
        put(&mut out, 32, &n64(at).to_le_bytes());
        put(&mut out, 40, &n64(block.len()).to_le_bytes());
        out.extend_from_slice(&block);
    }
    let flips = if u.ratio(1, 2)? {
        0
    } else {
        u.int_in_range(1..=3)?
    };
    for _ in 0..flips {
        let i = u.choose_index(out.len())?;
        out[i] ^= u.int_in_range(1..=255u8)?;
    }
    if flips > 0 && u.ratio(1, 2)? {
        seal(&mut out);
    }
    Ok(out)
}

/// A RELOCS section (§6.1, RELR): address words inside writable segments' content, and
/// bitmap words, now and then misaligned or first.
fn relocs(u: &mut Unstructured<'_>, writable: &[(u64, u64)]) -> Result<Plan> {
    let mut words = Vec::new();
    for _ in 0..u.int_in_range(1..=6)? {
        if words.is_empty() || u.ratio(2, 3)? {
            let &(rva, len) = u.choose(writable)?;
            let mut slot = rva + 8 * u.int_in_range(0..=(len / 8).saturating_sub(1))?;
            if u.ratio(1, 16)? {
                slot = slot.wrapping_add(u.arbitrary::<u64>()? & !1);
            }
            words.extend_from_slice(&slot.to_le_bytes());
        } else {
            let bits: u64 = u.arbitrary()?;
            words.extend_from_slice(&(bits | 1).to_le_bytes());
        }
    }
    Ok(Plan::plain(RELOCS, words, 0))
}

/// Lays `plans` out as §4.1 requires: header, table, zero padding to 4 KiB, then each section
/// at the end of the one before, aligned to 4 KiB if mapped and to 8 bytes if not; and seals
/// the section digests and the content digest.
fn layout(profile: u16, plans: &[Plan]) -> Vec<u8> {
    let count = plans.len();
    let mut out = vec![0u8; round_up(HEADER + count * ENTRY, PAGE)];
    let mut offsets = Vec::with_capacity(count);
    for p in plans {
        let align = if p.flags & MAPPABLE != 0 { PAGE } else { 8 };
        let at = round_up(out.len(), align);
        out.resize(at, 0);
        offsets.push(at);
        out.extend_from_slice(&p.stored);
    }
    put(&mut out, 0, &MAGIC);
    put(&mut out, 8, &1u16.to_le_bytes());
    put(&mut out, 12, &profile.to_le_bytes());
    put(&mut out, 14, &128u16.to_le_bytes());
    put(
        &mut out,
        20,
        &u32::try_from(count).unwrap_or(u32::MAX).to_le_bytes(),
    );
    put(&mut out, 24, &128u64.to_le_bytes());
    put(&mut out, 112, &1u16.to_le_bytes());
    for (i, (p, &at)) in plans.iter().zip(&offsets).enumerate() {
        let e = HEADER + i * ENTRY;
        let len = n64(p.stored.len());
        put(&mut out, e, &p.kind.to_le_bytes());
        put(&mut out, e + 4, &p.flags.to_le_bytes());
        put(&mut out, e + 8, &n64(at).to_le_bytes());
        put(&mut out, e + 16, &len.to_le_bytes());
        put(&mut out, e + 24, &len.to_le_bytes());
        put(&mut out, e + 32, &p.mem_offset.to_le_bytes());
        put(&mut out, e + 40, &p.mem_size.to_le_bytes());
        put(&mut out, e + 49, &[p.integrity]);
        put(&mut out, e + 52, &p.merkle_index.to_le_bytes());
        let digest = match p.integrity {
            0 => Sha256::digest(&p.stored),
            1 => onbf_merkle::root(&p.stored).unwrap_or([0; 32]),
            _ => [0; 32],
        };
        put(&mut out, e + 64, &digest);
    }
    content_digest(&mut out);
    out
}

/// The byte range `offset .. offset + size` of `b`, if it lies inside.
fn range_of(b: &[u8], offset: u64, size: u64) -> Option<std::ops::Range<usize>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(size).ok()?)?;
    (end <= b.len()).then_some(start..end)
}

/// Recomputes every digest of `b` that its section table lets the sealer find: SHA-256 for
/// integrity 0, the Merkle root and the MERKLE section's hash blocks for integrity 1, and then
/// the content digest (§4.5, §4.6). Entries whose range lies outside `b` are left as they are.
pub fn seal(b: &mut [u8]) {
    let count = le32(b, 20).map_or(0, |c| usize::try_from(c).unwrap_or(0).min(256));
    for i in 0..count {
        let e = HEADER + i * ENTRY;
        if e + ENTRY > b.len() {
            break;
        }
        let (Some(offset), Some(size)) = (le64(b, e + 8), le64(b, e + 16)) else {
            break;
        };
        let Some(range) = range_of(b, offset, size) else {
            continue;
        };
        let digest = match b[e + 49] {
            0 => Sha256::digest(&b[range]),
            1 => {
                let content = b[range].to_vec();
                if let Some(blocks) = hash_blocks_of(b, count, le32(b, e + 52), content.len()) {
                    let mut hb = vec![0u8; blocks.len()];
                    if onbf_merkle::build(&content, &mut hb).is_ok() {
                        b[blocks].copy_from_slice(&hb);
                    }
                }
                match onbf_merkle::root(&content) {
                    Ok(root) => root,
                    Err(_) => continue,
                }
            }
            _ => continue,
        };
        put(b, e + 64, &digest);
    }
    content_digest(b);
}

/// The range of the MERKLE section `index` names, if it holds exactly the hash blocks a
/// content of `content_len` bytes needs.
fn hash_blocks_of(
    b: &[u8],
    count: usize,
    index: Option<u32>,
    content_len: usize,
) -> Option<std::ops::Range<usize>> {
    let i = usize::try_from(index?).ok().filter(|&i| i < count)?;
    let e = HEADER + i * ENTRY;
    let r = range_of(b, le64(b, e + 8)?, le64(b, e + 16)?)?;
    let want = Shape::new(n64(content_len)).ok()?.hash_blocks_len();
    (n64(r.len()) == want && want > 0).then_some(r)
}

/// `content_digest = SHA-512(header[0..32] ‖ header[112..128] ‖ section_table)` (§4.5),
/// written into the header, if the table lies inside `b`.
fn content_digest(b: &mut [u8]) {
    let Some(count) = le32(b, 20) else { return };
    let Some(end) = usize::try_from(count)
        .ok()
        .and_then(|c| c.checked_mul(ENTRY))
        .and_then(|t| t.checked_add(HEADER))
        .filter(|&end| end <= b.len())
    else {
        return;
    };
    let mut h = Sha512::new();
    h.update(&b[0..32]);
    h.update(&b[112..128]);
    h.update(&b[HEADER..end]);
    put(b, 48, &h.finalize());
}

/// The parse options a caller can give: none, developer mode, nested in a verified parent.
const OPTIONS: [ParseOptions; 3] = [
    ParseOptions {
        nested_in_verified_parent: false,
        developer_mode: false,
    },
    ParseOptions {
        nested_in_verified_parent: false,
        developer_mode: true,
    },
    ParseOptions {
        nested_in_verified_parent: true,
        developer_mode: false,
    },
];

/// Parses `bytes` under each set of options, judges every image the parser accepts against
/// §4.1–§4.4, and runs every reader of an accepted image's parts.
///
/// # Panics
///
/// On a finding: an accepted image that breaks a rule of the container the parser promises.
pub fn exercise(bytes: &[u8]) {
    for options in OPTIONS {
        if let Ok(image) = Image::parse(bytes, options) {
            judge(&image, bytes, options);
            read_parts(&image);
        }
    }
}

/// The rules of §4.1–§4.4 an accepted image's public interface shows.
fn judge(image: &Image<'_>, bytes: &[u8], options: ParseOptions) {
    let count = image.section_count();
    assert!(
        (1..=256).contains(&count),
        "ONBF §4.2: an accepted image has 1 to 256 sections, not {count}"
    );
    let mut pos = round_up(HEADER + count * ENTRY, PAGE);
    assert!(
        pos <= bytes.len(),
        "ONBF §4.1: an accepted image holds its padded table"
    );
    let mut manifests = 0;
    let mut floor = 0u64;
    for i in 0..count {
        let s = image
            .section(i)
            .unwrap_or_else(|| panic!("ONBF: section {i} of an accepted image cannot be read"));
        let e = &s.entry;
        let align = if e.flags.has(Flags::MAPPABLE) {
            PAGE
        } else {
            8
        };
        let expected = round_up(pos, align);
        assert_eq!(
            usize::try_from(e.file_offset).ok(),
            Some(expected),
            "ONBF §4.1: section {i} of an accepted image does not start where the one before ends"
        );
        assert!(
            bytes[pos..expected].iter().all(|&x| x == 0),
            "ONBF §4.1: the padding before section {i} of an accepted image is not zero"
        );
        assert_eq!(
            n64(s.stored.len()),
            e.stored_size,
            "ONBF: section {i}'s bytes are not its size"
        );
        pos = expected + s.stored.len();
        assert!(
            e.flags.has(Flags::REQUIRED),
            "ONBF errata G5: section {i} of an accepted image is not REQUIRED"
        );
        assert!(
            !e.flags.has(Flags::WRITE | Flags::EXECUTE),
            "ONBF §4.3: section {i} of an accepted image is writable and executable"
        );
        match e.kind() {
            Kind::Manifest => manifests += 1,
            Kind::Segment => {
                assert!(
                    e.mem_offset >= floor
                        && e.mem_offset.is_multiple_of(PAGE64)
                        && e.mem_size.is_multiple_of(PAGE64)
                        && e.mem_size >= e.content_size,
                    "ONBF errata G4: segment {i} of an accepted image is out of order or not whole pages"
                );
                floor = e.mem_offset + e.mem_size;
                assert!(
                    floor <= MAX_SPAN,
                    "ONBF errata G4: an accepted image's span passes 2 GiB"
                );
            }
            _ => {}
        }
    }
    assert_eq!(
        manifests, 1,
        "ONBF §4.4: an accepted image has exactly one MANIFEST"
    );
    let h = image.header();
    match image.signature_block() {
        None => {
            assert_eq!(
                h.signature_block_offset, 0,
                "ONBF §4.1: the signature block lies outside the image"
            );
            assert!(
                options.developer_mode || options.nested_in_verified_parent,
                "ONBF §4.2: an unsigned image was accepted outside developer mode and nesting"
            );
            assert_eq!(
                pos,
                bytes.len(),
                "ONBF §4.1: an accepted unsigned image has bytes after its last section"
            );
        }
        Some(block) => {
            assert_eq!(
                usize::try_from(h.signature_block_offset).ok(),
                Some(round_up(pos, 8)),
                "ONBF §4.1: the signature block does not follow the last section"
            );
            assert_eq!(
                usize::try_from(h.signature_block_offset).unwrap_or(0) + block.len(),
                bytes.len(),
                "ONBF §4.1: the signature block does not end at the end of the file"
            );
        }
    }
}

/// Every reader of an accepted image's parts: the content digest, each section's digest or
/// Merkle tree (whole and block by block), the manifest, and the signature block.
fn read_parts(image: &Image<'_>) {
    let _ = image.verify_content_digest(|parts| {
        let mut h = Sha512::new();
        for p in parts {
            h.update(p);
        }
        h.finalize()
    });
    for s in image.sections() {
        match s.entry.integrity {
            Integrity::Sha256 => {
                let _ = s.verify_sha256(Sha256::digest);
            }
            Integrity::Merkle => merkle(image, &s),
            _ => {}
        }
    }
    if let Some(m) = image.manifest()
        && let Ok(manifest) = KrnManifest::decode(m.stored)
    {
        let _ = manifest.check_against(image);
    }
    if let Some(block) = image.signature_block() {
        let h = image.header();
        crate::signature::exercise(block, &h.content_digest, h.profile as u64);
    }
}

/// A Merkle-protected section's tree, whole and for its first blocks one at a time, as a
/// kernel verifies mapped pages lazily.
fn merkle(image: &Image<'_>, s: &Section<'_>) {
    let root = s.entry.digest;
    if s.entry.merkle_index == NO_MERKLE {
        let _ = onbf_merkle::root(s.stored).map(|r| r == root);
        return;
    }
    let Some(blocks) = usize::try_from(s.entry.merkle_index)
        .ok()
        .and_then(|i| image.section(i))
    else {
        return;
    };
    let _ = onbf_merkle::verify_tree(&root, s.stored, blocks.stored);
    if let Ok(shape) = Shape::new(n64(s.stored.len())) {
        let mut cache = NoCache;
        for (i, block) in s.stored.chunks(PAGE).take(4).enumerate() {
            let _ =
                onbf_merkle::verify_block(&root, &shape, blocks.stored, n64(i), block, &mut cache);
        }
    }
}

/// Seals `raw` (see [`seal`]), then runs the loader's paths on it: [`loader`].
pub fn exercise_loader(raw: &[u8]) {
    let mut b = raw.to_vec();
    seal(&mut b);
    loader(&b);
}

/// The UEFI loader's paths on `bytes`: `kernel::verify` with and without developer mode,
/// against the test kernel key, then the segment table and placement of what it accepts; and
/// placement of any image the container parser accepts in developer mode, which the loader
/// would not reach without a verified image but must still not fail on.
pub fn loader(bytes: &[u8]) {
    for developer_mode in [true, false] {
        if let Ok(verified) = kernel::verify(bytes, developer_mode, STORE) {
            place_all(&verified.image);
        }
    }
    if let Ok(image) = Image::parse(bytes, OPTIONS[1]) {
        place_all(&image);
    }
}

/// The loader's placement: the boot info's segment table, the span, and the image written
/// into a destination of that span with its relocations applied.
fn place_all(image: &Image<'_>) {
    let empty = KernelSegment {
        rva: 0,
        pages: 0,
        access: 0,
        reserved: 0,
    };
    let mut segments = [empty; MAX_KERNEL_SEGMENTS as usize];
    let _ = place::kernel_segments(image, &mut segments);
    let Ok(span) = place::span(image) else {
        return;
    };
    if span > MAX_PLACE {
        return;
    }
    let mut dest = vec![0u8; usize::try_from(span).unwrap_or(0)];
    let _ = place::place(image, &mut dest, BASE);
}

/// The `onbf_image_built` target: an image written from `data`'s choices, then every reader
/// of it and the loader's paths.
pub fn built(data: &[u8]) {
    let mut u = Unstructured::new(data);
    if let Ok(bytes) = image(&mut u) {
        exercise(&bytes);
        loader(&bytes);
    }
}
