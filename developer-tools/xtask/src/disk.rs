//! The NVMe test disk the boot test attaches: a GPT disk (UEFI 2.11 §5.3) with two
//! partitions and a known pattern in the first, so the kernel's NVMe driver and GPT parser
//! can be checked against bytes written here, independently of them.
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
pub(crate) const DISK_GUID: &str = "28a71590-fa80-422c-a428-8dff96984d99";
const PARTITION_GUIDS: [&str; 2] = [
    "2afdfd45-f1b7-48eb-a489-f495b3541901",
    "2afdfd45-f1b7-48eb-a489-f495b3541902",
];

/// The partitions: name, type, first and last block.
pub(crate) const PARTITIONS: [(&str, &str, u64, u64); 2] = [
    ("Origin system A", ORIGIN_SYSTEM, 2048, 10239),
    (
        "Origin data",
        ORIGIN_DATA,
        10240,
        BLOCKS - 1 - 1 - ENTRY_BLOCKS,
    ),
];

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

/// The partition entry array.
fn entries() -> Vec<u8> {
    let mut array = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
    for (i, (name, kind, first, last)) in PARTITIONS.iter().enumerate() {
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

/// The whole disk image.
pub(crate) fn image() -> Vec<u8> {
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
    let array = entries();
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
    fs::write(path, image()).map_err(|e| format!("{}: {e}", path.display()))
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
        assert_eq!(PARTITIONS[1].3, BLOCKS - 34, "the last usable block");
    }
}
