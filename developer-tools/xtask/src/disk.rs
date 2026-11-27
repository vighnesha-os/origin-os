//! The NVMe test disk the boot test attaches: a GPT disk (UEFI 2.11 §5.3) with three
//! partitions, a known pattern in the first, and the mark of Origin's tests on the third, the
//! only blocks the boot proofs may write (TASKS item 171). The kernel's NVMe driver and GPT
//! parser are checked against bytes written here, independently of them, and after each boot
//! the image is compared with what was written here ([`check_writes`]).
//!
//! The CRC-32 here is the bitwise form of the IEEE 802.3 polynomial; the kernel has its own
//! table-driven one. Both are checked against the standard check value.

use std::fs;
use std::path::Path;

/// Bytes per logical block.
pub(crate) const BLOCK: u64 = 512;
/// Blocks on the disk: 16 MiB.
pub(crate) const BLOCKS: u64 = 32768;
/// GPT partition entries, and their size.
const ENTRIES: u64 = 128;
const ENTRY_SIZE: u64 = 128;
/// Blocks the entry array takes: 128 × 128 bytes.
const ENTRY_BLOCKS: u64 = ENTRIES * ENTRY_SIZE / BLOCK;

/// Origin partition type GUIDs. Provisional: generated for this test disk; the installer's
/// partition-layout decision (M9) fixes the real ones.
pub(crate) const ORIGIN_SYSTEM: &str = "ab846206-03ad-4cf6-b80b-5da764646190";
pub(crate) const ORIGIN_DATA: &str = "1a0f3667-a459-431f-b1fb-c1d11e0d699a";
/// The mark of Origin's tests: the type of the partition the boot proofs may overwrite (TASKS
/// item 171). PROVISIONAL: generated (UUID version 4) on 2026-11-27 and accepted the same day by
/// the owner, owner item 384, as the provisional type on test disks only, to be fixed with M9's
/// partition-layout decision. The kernel's `blockcache::test_disk::MARK` holds its bytes, and a
/// host test there ties the two.
pub(crate) const ORIGIN_PROOF_SCRATCH: &str = "8d4070b7-7072-4d27-9689-81467d6a5735";
pub(crate) const DISK_GUID: &str = "28a71590-fa80-422c-a428-8dff96984d99";
pub(crate) const PARTITION_GUIDS: [&str; 3] = [
    "2afdfd45-f1b7-48eb-a489-f495b3541901",
    "2afdfd45-f1b7-48eb-a489-f495b3541902",
    "2afdfd45-f1b7-48eb-a489-f495b3541903",
];

/// The last usable block: the backup's entry array and header follow it.
pub(crate) const LAST_USABLE: u64 = BLOCKS - 1 - 1 - ENTRY_BLOCKS;

/// The partitions: name, type, first and last block. The third carries the mark.
pub(crate) const PARTITIONS: [(&str, &str, u64, u64); 3] = [
    ("Origin system A", ORIGIN_SYSTEM, 2048, 10239),
    ("Origin data", ORIGIN_DATA, 10240, 18431),
    (
        "Origin boot-proof scratch",
        ORIGIN_PROOF_SCRATCH,
        18432,
        LAST_USABLE,
    ),
];

/// The marked partition's first and last blocks on the disk the boot test builds.
pub(crate) const MARKED: (u64, u64) = (PARTITIONS[2].2, PARTITIONS[2].3);

/// Where the boot proofs write in the marked partition (TASKS item 171), as the kernel's
/// `blockcache::test_disk::plan` computes it from the GPT; a host test there ties the two. The
/// block cache's page of 8 blocks, the reserved-memory proof's 8 blocks after it, each
/// processor's block from 64 blocks in, one page apart, and the polled proof's at the end.
pub(crate) const CACHE_LBA: u64 = MARKED.0;
pub(crate) const RESERVED_LBA: u64 = CACHE_LBA + 8;
pub(crate) const POLLED_LBA: u64 = MARKED.1;
pub(crate) const fn cpu_lba(k: u64) -> u64 {
    CACHE_LBA + 64 + 8 * k
}

/// The disks the boot test attaches: the marked one every kernel boot gets, and the controls of
/// TASKS item 171 (ADR-0013's controls D1 and D2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Variant {
    /// [`PARTITIONS`] as they are.
    Marked,
    /// The third partition's type is Origin data's: everything else the same, and no mark (D1).
    Unmarked,
    /// The third partition is 72 blocks, [`SMALL_LAST`] its end, the rest of the disk free:
    /// room for every fixed write and for processor 0's block, not processor 1's (D2).
    Small,
}

/// The end of [`Variant::Small`]'s third partition.
pub(crate) const SMALL_LAST: u64 = MARKED.0 + 71;

impl Variant {
    /// Its partitions.
    pub(crate) fn partitions(self) -> [(&'static str, &'static str, u64, u64); 3] {
        let mut parts = PARTITIONS;
        match self {
            Self::Marked => {}
            Self::Unmarked => parts[2].1 = ORIGIN_DATA,
            Self::Small => parts[2].3 = SMALL_LAST,
        }
        parts
    }

    /// The marked partition's first and last blocks, if it has one.
    pub(crate) fn marked(self) -> Option<(u64, u64)> {
        let parts = self.partitions();
        (parts[2].1 == ORIGIN_PROOF_SCRATCH).then_some((parts[2].2, parts[2].3))
    }
}

/// Blocks of the pattern at the start of the first partition.
pub(crate) const PATTERN_BLOCKS: u64 = 64;

/// The pattern of block `lba`: `ORIGIN NVME LBA 00002048 ` repeated, cut to one block.
/// The kernel computes the same bytes to check what it reads.
pub(crate) fn pattern(lba: u64) -> Vec<u8> {
    let unit = format!("ORIGIN NVME LBA {lba:08} ");
    unit.bytes().cycle().take(BLOCK as usize).collect()
}

/// CRC-32 (IEEE 802.3, reflected, initial and final value all ones), bit by bit.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A GUID in its on-disk (mixed-endian) form: the first three fields little-endian, the
/// last two as written.
pub(crate) fn guid(text: &str) -> [u8; 16] {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let mut b = [0u8; 16];
    for (i, out) in b.iter_mut().enumerate() {
        *out = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or(0);
    }
    b[0..4].reverse();
    b[4..6].reverse();
    b[6..8].reverse();
    b
}

fn put(disk: &mut [u8], at: u64, bytes: &[u8]) {
    let at = at as usize;
    disk[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The partition entry array of `parts`.
fn entries(parts: &[(&str, &str, u64, u64)]) -> Vec<u8> {
    let mut array = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
    for (i, (name, kind, first, last)) in parts.iter().enumerate() {
        let e = &mut array[i * ENTRY_SIZE as usize..(i + 1) * ENTRY_SIZE as usize];
        e[0..16].copy_from_slice(&guid(kind));
        e[16..32].copy_from_slice(&guid(PARTITION_GUIDS[i]));
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        for (j, unit) in name.encode_utf16().enumerate() {
            e[56 + 2 * j..58 + 2 * j].copy_from_slice(&unit.to_le_bytes());
        }
    }
    array
}

/// A GPT header at `mine`, whose other copy is at `other` and whose entries start at
/// `entries_at`.
fn header(mine: u64, other: u64, entries_at: u64, entries_crc: u32) -> Vec<u8> {
    let mut h = vec![0u8; BLOCK as usize];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&mine.to_le_bytes());
    h[32..40].copy_from_slice(&other.to_le_bytes());
    h[40..48].copy_from_slice(&(2 + ENTRY_BLOCKS).to_le_bytes());
    h[48..56].copy_from_slice(&(BLOCKS - 2 - ENTRY_BLOCKS).to_le_bytes());
    h[56..72].copy_from_slice(&guid(DISK_GUID));
    h[72..80].copy_from_slice(&entries_at.to_le_bytes());
    h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
    h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32(&h[..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// The whole disk image every kernel boot gets: [`Variant::Marked`].
#[cfg(test)]
pub(crate) fn image() -> Vec<u8> {
    image_with(Variant::Marked)
}

/// The whole disk image of `variant`.
pub(crate) fn image_with(variant: Variant) -> Vec<u8> {
    let mut disk = vec![0u8; (BLOCKS * BLOCK) as usize];
    // Protective MBR: one partition of type EEh covering the disk from block 1.
    let mut mbr = [0u8; 16];
    mbr[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr[4] = 0xEE;
    mbr[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    mbr[8..12].copy_from_slice(&1u32.to_le_bytes());
    mbr[12..16].copy_from_slice(&((BLOCKS - 1) as u32).to_le_bytes());
    put(&mut disk, 446, &mbr);
    put(&mut disk, 510, &[0x55, 0xAA]);
    let array = entries(&variant.partitions());
    let array_crc = crc32(&array);
    let last = BLOCKS - 1;
    let backup_entries = last - ENTRY_BLOCKS;
    put(&mut disk, BLOCK, &header(1, last, 2, array_crc));
    put(&mut disk, 2 * BLOCK, &array);
    put(&mut disk, backup_entries * BLOCK, &array);
    put(
        &mut disk,
        last * BLOCK,
        &header(last, 1, backup_entries, array_crc),
    );
    let first = PARTITIONS[0].2;
    for lba in first..first + PATTERN_BLOCKS {
        put(&mut disk, lba * BLOCK, &pattern(lba));
    }
    disk
}

/// Writes a fresh image to `path` (so nothing a previous boot wrote survives).
pub(crate) fn write_image(path: &Path) -> Result<(), String> {
    write_image_with(path, Variant::Marked)
}

/// Writes a fresh image of `variant` to `path`.
pub(crate) fn write_image_with(path: &Path, variant: Variant) -> Result<(), String> {
    fs::write(path, image_with(variant)).map_err(|e| format!("{}: {e}", path.display()))
}

/// The blocks in which `after` differs from `before`, in order, and whether the two differ in
/// length.
pub(crate) fn changed_blocks(before: &[u8], after: &[u8]) -> (Vec<u64>, bool) {
    let changed = before
        .chunks(BLOCK as usize)
        .zip(after.chunks(BLOCK as usize))
        .enumerate()
        .filter(|(_, (b, a))| b != a)
        .map(|(i, _)| i as u64)
        .collect();
    (changed, before.len() != after.len())
}

/// Runs of consecutive blocks, `a-b`, for a message.
fn runs(blocks: &[u64]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < blocks.len() {
        let first = blocks[i];
        let mut last = first;
        while i + 1 < blocks.len() && blocks[i + 1] == last + 1 {
            i += 1;
            last = blocks[i];
        }
        out.push(if first == last {
            format!("{first}")
        } else {
            format!("{first}-{last}")
        });
        i += 1;
    }
    let shown = out.len().min(8);
    let mut text = out[..shown].join(", ");
    if out.len() > shown {
        text.push_str(&format!(" and {} more runs", out.len() - shown));
    }
    text
}

/// What a boot may have done to `variant`'s image, judged from its bytes after the boot (TASKS
/// item 171): no block changed outside the marked partition, and none at all on a disk without
/// one; with `wrote`, at least one block inside it changed, as the write proofs' markers say
/// they wrote.
pub(crate) fn check_writes(variant: Variant, after: &[u8], wrote: bool) -> Result<(), String> {
    let before = image_with(variant);
    let (changed, resized) = changed_blocks(&before, after);
    if resized {
        return Err(format!(
            "the NVMe test disk is {} bytes after the boot, not the {} written before it",
            after.len(),
            before.len()
        ));
    }
    let inside = |lba: &u64| variant.marked().is_some_and(|(f, l)| (f..=l).contains(lba));
    let outside: Vec<u64> = changed.iter().copied().filter(|b| !inside(b)).collect();
    if !outside.is_empty() {
        return Err(format!(
            "the NVMe test disk changed outside {}: blocks {}",
            match variant.marked() {
                Some((f, l)) => format!("the marked partition (blocks {f}-{l})"),
                None => "any marked partition, for it has none".into(),
            },
            runs(&outside)
        ));
    }
    if wrote && changed.is_empty() {
        return Err(
            "the NVMe test disk is unchanged after a boot whose write proofs say they wrote to it"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn guid_byte_order() {
        // The EFI System Partition type, as UEFI 2.11 Table 5-7 lists its bytes on disk.
        assert_eq!(
            guid("c12a7328-f81f-11d2-ba4b-00a0c93ec93b"),
            [
                0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
                0xC9, 0x3B
            ]
        );
    }

    #[test]
    fn the_image_is_a_consistent_gpt_disk() {
        let d = image();
        assert_eq!(d.len() as u64, BLOCKS * BLOCK);
        assert_eq!(&d[510..512], &[0x55, 0xAA]);
        assert_eq!(d[446 + 4], 0xEE);
        let h = &d[512..1024];
        assert_eq!(&h[0..8], b"EFI PART");
        let mut check = h[..92].to_vec();
        check[16..20].fill(0);
        assert_eq!(crc32(&check).to_le_bytes(), h[16..20]);
        let array = &d[1024..1024 + 16384];
        assert_eq!(crc32(array).to_le_bytes(), h[88..92]);
        let backup = &d[(BLOCKS as usize - 1) * 512..];
        assert_eq!(
            &backup[24..32],
            &(BLOCKS - 1).to_le_bytes(),
            "the backup's own LBA"
        );
        assert_eq!(&backup[32..40], &1u64.to_le_bytes());
        let lba = 2048;
        assert_eq!(&d[lba * 512..lba * 512 + 25], b"ORIGIN NVME LBA 00002048 ");
        assert_eq!(PARTITIONS[2].3, BLOCKS - 34, "the last usable block");
        assert_eq!(LAST_USABLE, BLOCKS - 34);
        assert_eq!(
            PARTITIONS[1].3 + 1,
            PARTITIONS[2].2,
            "data, then the marked partition"
        );
    }

    /// TASKS item 171: the variants differ from the marked disk where they say and nowhere else,
    /// each a consistent GPT disk.
    #[test]
    fn the_control_disks_differ_only_where_they_say() {
        let marked = image();
        assert_eq!(Variant::Marked.marked(), Some(MARKED));
        assert_eq!(Variant::Unmarked.marked(), None);
        assert_eq!(Variant::Small.marked(), Some((18432, 18503)));
        for v in [Variant::Unmarked, Variant::Small] {
            let d = image_with(v);
            let (changed, resized) = changed_blocks(&marked, &d);
            assert!(!resized);
            // Only the two headers and the two entry arrays' first blocks (the third entry).
            assert_eq!(changed, vec![1, 2, BLOCKS - 33, BLOCKS - 1], "{v:?}");
            // Each still a GPT disk whose CRCs hold.
            let h = &d[512..1024];
            let mut check = h[..92].to_vec();
            check[16..20].fill(0);
            assert_eq!(crc32(&check).to_le_bytes(), h[16..20], "{v:?}");
            assert_eq!(
                crc32(&d[1024..1024 + 16384]).to_le_bytes(),
                h[88..92],
                "{v:?}"
            );
        }
        // The write plan lies in the marked partition, processor 0's block in the small one's
        // and processor 1's just past it.
        let inside = |lba: u64, (f, l): (u64, u64)| (f..=l).contains(&lba);
        for lba in [
            CACHE_LBA,
            CACHE_LBA + 7,
            RESERVED_LBA,
            RESERVED_LBA + 7,
            POLLED_LBA,
            cpu_lba(0),
            cpu_lba(1779),
        ] {
            assert!(inside(lba, MARKED), "{lba}");
        }
        // Room for 1,780 processors' blocks, 0 to 1779, before the polled proof's.
        assert!(cpu_lba(1779) < POLLED_LBA && cpu_lba(1780) >= POLLED_LBA);
        let small = (MARKED.0, SMALL_LAST);
        assert!(inside(cpu_lba(0), small) && !inside(cpu_lba(1), small));
        assert!(inside(RESERVED_LBA + 7, small) && cpu_lba(0) < SMALL_LAST);
    }

    #[test]
    fn the_byte_check_sees_writes_outside_the_marked_partition() {
        let mut d = image();
        assert_eq!(check_writes(Variant::Marked, &d, false), Ok(()));
        assert!(
            check_writes(Variant::Marked, &d, true)
                .is_err_and(|e| e.contains("unchanged after a boot whose write proofs")),
        );
        // A block inside: fine either way.
        d[(CACHE_LBA * BLOCK) as usize] ^= 1;
        d[(POLLED_LBA * BLOCK + 511) as usize] ^= 1;
        assert_eq!(check_writes(Variant::Marked, &d, true), Ok(()));
        // One block before it, one after it (the backup's entry array), and block 0.
        for lba in [MARKED.0 - 1, MARKED.1 + 1, 0] {
            let mut e = d.clone();
            e[(lba * BLOCK) as usize] ^= 1;
            let err = check_writes(Variant::Marked, &e, true).unwrap_err();
            assert!(
                err.contains(&format!("blocks {lba}")) && err.contains("blocks 18432-32734"),
                "{err}"
            );
        }
        // A disk without the mark: any change at all.
        let mut u = image_with(Variant::Unmarked);
        assert_eq!(check_writes(Variant::Unmarked, &u, false), Ok(()));
        u[(CACHE_LBA * BLOCK) as usize] ^= 1;
        u[(CACHE_LBA * BLOCK + 512 * 3) as usize] ^= 1;
        u[(CACHE_LBA * BLOCK + 512 * 4) as usize] ^= 1;
        assert_eq!(
            check_writes(Variant::Unmarked, &u, false),
            Err("the NVMe test disk changed outside any marked partition, for it has none: blocks 18432, 18435-18436".into())
        );
        // The small partition: processor 1's block is outside it.
        let mut s = image_with(Variant::Small);
        s[(cpu_lba(0) * BLOCK) as usize] ^= 1;
        assert_eq!(check_writes(Variant::Small, &s, true), Ok(()));
        s[(cpu_lba(1) * BLOCK) as usize] ^= 1;
        assert_eq!(
            check_writes(Variant::Small, &s, true),
            Err("the NVMe test disk changed outside the marked partition (blocks 18432-18503): blocks 18504".into())
        );
        // A disk that changed length.
        assert!(
            check_writes(Variant::Marked, &d[..512], false)
                .is_err_and(|e| e.contains("bytes after the boot"))
        );
    }
}
