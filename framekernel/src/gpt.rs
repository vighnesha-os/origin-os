//! GUID Partition Tables (CLAUDE.md §39 partition tables; UEFI 2.11 §5.3): header and
//! entry-array validation and the partitions they describe.
//!
//! A disk is untrusted input. The checks follow UEFI 2.11 §5.3.2:
//! - the signature, the revision and a header size from 92 bytes to one block;
//! - the header CRC-32 (over the header size, with the CRC field zeroed);
//! - MyLBA is the block the header was read from;
//! - the usable range lies inside the disk, after the primary structures and before the
//!   backup ones;
//! - the entry array's CRC-32.
//!
//! On top of that, every used entry must lie inside the usable range, and no two may
//! overlap. The backup header, at the last block, must describe the same disk. The caller
//! decides what to trust when only one copy is valid.

use core::fmt;

/// CRC-32 (IEEE 802.3, reflected polynomial EDB88320h, initial and final value all ones),
/// table-driven.
pub fn crc32(data: &[u8]) -> u32 {
    !crc32_update(0xFFFF_FFFF, data)
}

/// Continues a CRC-32 over `data` from the running (not yet inverted) value `crc`.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        let index = usize::from((crc as u8) ^ b);
        crc = (crc >> 8) ^ CRC_TABLE.get(index).copied().unwrap_or(0);
    }
    crc
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// A GUID, as stored on disk (the first three fields little-endian).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    pub fn is_zero(&self) -> bool {
        self.0 == [0; 16]
    }
}

/// The canonical text form, `ab846206-03ad-4cf6-b80b-5da764646190`.
impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9]
        )?;
        for x in &b[10..16] {
            write!(f, "{x:02x}")?;
        }
        Ok(())
    }
}

/// Why a header or entry array was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GptError {
    Signature,
    Revision(u32),
    HeaderSize(u32),
    HeaderCrc,
    /// MyLBA is not the block the header came from.
    MyLba(u64),
    /// The usable range, the alternate header or the entry array lies outside the disk or
    /// overlaps the other.
    Layout,
    /// The entry count or size is unsupported (size not 128 × 2^n, or the array is larger
    /// than [`MAX_ARRAY`]).
    Entries,
    ArrayCrc,
    /// Entry `index` lies outside the usable range or ends before it starts.
    Partition(u32),
    /// Entries `a` and `b` overlap.
    Overlap(u32, u32),
    /// The two headers describe different disks.
    Mismatch,
}

/// The largest entry array accepted: 128 entries of 128 bytes is the usual 16 KiB; this
/// allows four times that.
pub const MAX_ARRAY: u64 = 64 << 10;

/// A validated header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub my_lba: u64,
    pub alternate_lba: u64,
    pub first_usable: u64,
    pub last_usable: u64,
    pub disk: Guid,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc: u32,
}

impl Header {
    /// Bytes of the entry array.
    pub const fn array_len(&self) -> u64 {
        self.entry_count as u64 * self.entry_size as u64
    }

    /// Blocks the entry array takes.
    pub const fn array_blocks(&self, block: u64) -> u64 {
        self.array_len().div_ceil(block)
    }

    /// Whether `other` (the other copy) describes the same disk and partitions.
    pub fn agrees_with(&self, other: &Header) -> bool {
        self.disk == other.disk
            && self.first_usable == other.first_usable
            && self.last_usable == other.last_usable
            && self.entry_count == other.entry_count
            && self.entry_size == other.entry_size
            && self.entries_crc == other.entries_crc
            && self.alternate_lba == other.my_lba
            && other.alternate_lba == self.my_lba
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut v = [0u8; 4];
    if let Some(s) = b.get(at..at + 4) {
        v.copy_from_slice(s);
    }
    u32::from_le_bytes(v)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    if let Some(s) = b.get(at..at + 8) {
        v.copy_from_slice(s);
    }
    u64::from_le_bytes(v)
}

/// Validates the header in `block`, read from block `lba` of a disk of `disk_blocks`
/// blocks of `block.len()` bytes.
pub fn parse_header(block: &[u8], lba: u64, disk_blocks: u64) -> Result<Header, GptError> {
    if block.get(0..8) != Some(&b"EFI PART"[..]) {
        return Err(GptError::Signature);
    }
    let revision = u32_at(block, 8);
    if revision != 0x0001_0000 {
        return Err(GptError::Revision(revision));
    }
    let size = u32_at(block, 12);
    if size < 92 || size as usize > block.len() {
        return Err(GptError::HeaderSize(size));
    }
    let stored = u32_at(block, 16);
    let mut crc = crc32_update(0xFFFF_FFFF, block.get(0..16).unwrap_or(&[]));
    crc = crc32_update(crc, &[0; 4]);
    crc = crc32_update(crc, block.get(20..size as usize).unwrap_or(&[]));
    if !crc != stored {
        return Err(GptError::HeaderCrc);
    }
    let mut disk = [0u8; 16];
    disk.copy_from_slice(block.get(56..72).unwrap_or(&[0; 16]));
    let h = Header {
        my_lba: u64_at(block, 24),
        alternate_lba: u64_at(block, 32),
        first_usable: u64_at(block, 40),
        last_usable: u64_at(block, 48),
        disk: Guid(disk),
        entries_lba: u64_at(block, 72),
        entry_count: u32_at(block, 80),
        entry_size: u32_at(block, 84),
        entries_crc: u32_at(block, 88),
    };
    if h.my_lba != lba {
        return Err(GptError::MyLba(h.my_lba));
    }
    let size_ok = h.entry_size >= 128 && h.entry_size.is_power_of_two();
    if !size_ok || h.entry_count == 0 || h.array_len() > MAX_ARRAY {
        return Err(GptError::Entries);
    }
    let bs = block.len() as u64;
    let array_end = h
        .entries_lba
        .checked_add(h.array_blocks(bs))
        .ok_or(GptError::Layout)?;
    let last = disk_blocks.checked_sub(1).ok_or(GptError::Layout)?;
    // The usable range lies between the two copies' structures, and the array lies outside
    // it, inside the disk, never over block 0 or a header.
    let usable = h.first_usable <= h.last_usable && h.last_usable < last && h.first_usable > 1;
    let clear_of_usable = |lba: u64| lba < h.first_usable || lba > h.last_usable;
    let alternate = h.alternate_lba <= last
        && h.my_lba <= last
        && h.alternate_lba != h.my_lba
        && clear_of_usable(h.alternate_lba)
        && clear_of_usable(h.my_lba);
    let outside = array_end <= h.first_usable || h.entries_lba > h.last_usable;
    // Block 0 is the protective MBR.
    let array_ok = h.entries_lba >= 1
        && array_end <= disk_blocks
        && outside
        && !(h.entries_lba..array_end).contains(&h.my_lba)
        && !(h.entries_lba..array_end).contains(&h.alternate_lba);
    if !(usable && alternate && array_ok) {
        return Err(GptError::Layout);
    }
    Ok(h)
}

/// One used partition entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    /// Its index in the entry array.
    pub index: u32,
    pub kind: Guid,
    pub unique: Guid,
    pub first: u64,
    pub last: u64,
    pub attributes: u64,
    /// The name, UTF-16LE, up to 36 code units, cut at the first NUL.
    pub name: [u16; 36],
}

impl Partition {
    pub const fn blocks(&self) -> u64 {
        self.last - self.first + 1
    }

    /// The name as text (unpaired surrogates shown as U+FFFD).
    pub fn name(&self) -> impl fmt::Display + '_ {
        Name(&self.name)
    }
}

struct Name<'a>(&'a [u16; 36]);

impl fmt::Display for Name<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let units = self.0.iter().copied().take_while(|&u| u != 0);
        for c in char::decode_utf16(units) {
            fmt::Write::write_char(f, c.unwrap_or(char::REPLACEMENT_CHARACTER))?;
        }
        Ok(())
    }
}

/// Checks the entry array (exactly [`Header::array_len`] bytes, or more) against `h` and
/// lists its used entries into `out`; returns how many there are, or `Err` with
/// [`GptError::Entries`] if `out` is too small.
pub fn parse_entries(array: &[u8], h: &Header, out: &mut [Partition]) -> Result<usize, GptError> {
    // A `Header` built by hand need not have passed `parse_header`.
    if h.entry_size < 128 {
        return Err(GptError::Entries);
    }
    let len = usize::try_from(h.array_len()).map_err(|_| GptError::Entries)?;
    let array = array.get(..len).ok_or(GptError::Entries)?;
    if crc32(array) != h.entries_crc {
        return Err(GptError::ArrayCrc);
    }
    let mut n = 0;
    for (i, e) in array.chunks_exact(h.entry_size as usize).enumerate() {
        let index = i as u32;
        let mut kind = [0u8; 16];
        kind.copy_from_slice(e.get(0..16).unwrap_or(&[0; 16]));
        if kind == [0; 16] {
            continue;
        }
        let mut unique = [0u8; 16];
        unique.copy_from_slice(e.get(16..32).unwrap_or(&[0; 16]));
        let (first, last) = (u64_at(e, 32), u64_at(e, 40));
        if first > last || first < h.first_usable || last > h.last_usable {
            return Err(GptError::Partition(index));
        }
        let mut name = [0u16; 36];
        for (j, unit) in name.iter_mut().enumerate() {
            let at = 56 + 2 * j;
            *unit = u16::from_le_bytes([
                e.get(at).copied().unwrap_or(0),
                e.get(at + 1).copied().unwrap_or(0),
            ]);
        }
        let p = Partition {
            index,
            kind: Guid(kind),
            unique: Guid(unique),
            first,
            last,
            attributes: u64_at(e, 48),
            name,
        };
        if let Some(other) = out
            .get(..n)
            .and_then(|seen| seen.iter().find(|q| q.first <= p.last && p.first <= q.last))
        {
            return Err(GptError::Overlap(other.index, index));
        }
        *out.get_mut(n).ok_or(GptError::Entries)? = p;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BS: usize = 512;
    const DISK: u64 = 4096;

    fn guid(seed: u8) -> [u8; 16] {
        core::array::from_fn(|i| seed.wrapping_add(i as u8))
    }

    fn entries(parts: &[(u8, u64, u64, &str)]) -> Vec<u8> {
        let mut a = vec![0u8; 128 * 128];
        for (i, (seed, first, last, name)) in parts.iter().enumerate() {
            let e = &mut a[i * 128..(i + 1) * 128];
            e[0..16].copy_from_slice(&guid(*seed));
            e[16..32].copy_from_slice(&guid(seed + 100));
            e[32..40].copy_from_slice(&first.to_le_bytes());
            e[40..48].copy_from_slice(&last.to_le_bytes());
            for (j, u) in name.encode_utf16().enumerate() {
                e[56 + 2 * j..58 + 2 * j].copy_from_slice(&u.to_le_bytes());
            }
        }
        a
    }

    fn header(mine: u64, other: u64, entries_lba: u64, array: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; BS];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&mine.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&(DISK - 34).to_le_bytes());
        h[56..72].copy_from_slice(&guid(7));
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&crc32(array).to_le_bytes());
        seal(&mut h);
        h
    }

    fn seal(h: &mut [u8]) {
        h[16..20].fill(0);
        let c = crc32(&h[..92]);
        h[16..20].copy_from_slice(&c.to_le_bytes());
    }

    #[test]
    fn crc32_check_value_and_increments() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        let split = !crc32_update(crc32_update(0xFFFF_FFFF, b"1234"), b"56789");
        assert_eq!(split, 0xCBF4_3926);
    }

    #[test]
    fn guid_text_form() {
        let esp = Guid([
            0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E,
            0xC9, 0x3B,
        ]);
        assert_eq!(format!("{esp}"), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
        assert!(Guid::default().is_zero() && !esp.is_zero());
    }

    #[test]
    fn a_valid_disk_lists_its_partitions_and_both_headers_agree() {
        let array = entries(&[
            (1, 2048, 2999, "Origin system A"),
            (2, 3000, DISK - 34, "Origin data"),
        ]);
        let primary = parse_header(&header(1, DISK - 1, 2, &array), 1, DISK).unwrap();
        let backup = parse_header(&header(DISK - 1, 1, DISK - 33, &array), DISK - 1, DISK).unwrap();
        assert!(primary.agrees_with(&backup) && backup.agrees_with(&primary));
        let mut out = [Partition {
            index: 0,
            kind: Guid::default(),
            unique: Guid::default(),
            first: 0,
            last: 0,
            attributes: 0,
            name: [0; 36],
        }; 4];
        let n = parse_entries(&array, &primary, &mut out).unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            (out[0].first, out[0].last, out[0].blocks()),
            (2048, 2999, 952)
        );
        assert_eq!(format!("{}", out[0].name()), "Origin system A");
        assert_eq!(format!("{}", out[1].name()), "Origin data");
        assert_eq!(out[1].kind, Guid(guid(2)));
        assert_eq!(primary.array_blocks(512), 32);
    }

    #[test]
    fn header_checks() {
        let array = entries(&[(1, 2048, 2999, "a")]);
        let good = header(1, DISK - 1, 2, &array);
        let edit = |f: &dyn Fn(&mut Vec<u8>), reseal: bool| {
            let mut h = good.clone();
            f(&mut h);
            if reseal {
                seal(&mut h);
            }
            parse_header(&h, 1, DISK)
        };
        assert_eq!(edit(&|h| h[0] = b'X', true), Err(GptError::Signature));
        assert_eq!(
            edit(&|h| h[10] = 2, true),
            Err(GptError::Revision(0x0002_0000))
        );
        assert_eq!(edit(&|h| h[12] = 91, true), Err(GptError::HeaderSize(91)));
        assert_eq!(
            edit(&|h| h[100] = 1, false),
            Ok(parse_header(&good, 1, DISK).unwrap()),
            "bytes past the header size are not covered"
        );
        assert_eq!(edit(&|h| h[60] ^= 1, false), Err(GptError::HeaderCrc));
        assert_eq!(parse_header(&good, 2, DISK), Err(GptError::MyLba(1)));
        assert_eq!(
            edit(&|h| h[84] = 100, true),
            Err(GptError::Entries),
            "entry size not a power of two"
        );
        assert_eq!(
            edit(&|h| h[81] = 0x10, true),
            Err(GptError::Entries),
            "an array over 64 KiB"
        );
        assert_eq!(
            edit(&|h| h[48..56].copy_from_slice(&DISK.to_le_bytes()), true),
            Err(GptError::Layout),
            "past the disk"
        );
        assert_eq!(
            edit(&|h| h[40..48].copy_from_slice(&10u64.to_le_bytes()), true),
            Err(GptError::Layout),
            "usable range over the array"
        );
        assert_eq!(
            edit(&|h| h[32..40].copy_from_slice(&1u64.to_le_bytes()), true),
            Err(GptError::Layout),
            "alternate is itself"
        );
        assert_eq!(
            parse_header(&good, 1, 1),
            Err(GptError::Layout),
            "a one-block disk"
        );
    }

    /// Whatever a disk holds, parsing never panics, and anything accepted keeps the
    /// invariants: a header whose CRC really matches, a usable range inside the disk, and
    /// partitions inside it that never overlap.
    #[test]
    fn mutations_never_panic_and_accepted_tables_keep_their_invariants() {
        use crate::testing::{Rng, mutate};
        let none = Partition {
            index: 0,
            kind: Guid::default(),
            unique: Guid::default(),
            first: 0,
            last: 0,
            attributes: 0,
            name: [0; 36],
        };
        let array = entries(&[
            (1, 2048, 2999, "Origin system A"),
            (2, 3000, DISK - 34, "Origin data"),
        ]);
        let good = header(1, DISK - 1, 2, &array);
        let mut rng = Rng::new(0x6770_7431);
        let (mut headers, mut tables) = (0, 0);
        for case in 0..crate::testing::cases(4000) {
            let mut h = good.clone();
            let mut a = array.clone();
            if case % 2 == 0 {
                mutate(&mut h, &mut rng);
            } else {
                mutate(&mut a, &mut rng);
            }
            // Half the cases re-seal both CRCs, so the checks behind them are reached.
            if case % 4 >= 2 && h.len() >= 92 && a.len() >= 16384 {
                let crc = crc32(&a[..16384]);
                h[88..92].copy_from_slice(&crc.to_le_bytes());
                seal(&mut h);
            }
            let Ok(parsed) = parse_header(&h, 1, DISK) else {
                continue;
            };
            headers += 1;
            let size = u32_at(&h, 12) as usize;
            let mut check = h[..size].to_vec();
            check[16..20].fill(0);
            assert_eq!(crc32(&check), u32_at(&h, 16), "case {case}");
            assert!(
                parsed.first_usable <= parsed.last_usable && parsed.last_usable < DISK - 1,
                "case {case}"
            );
            let mut out = [none; 8];
            if let Ok(n) = parse_entries(&a, &parsed, &mut out) {
                tables += 1;
                let parts = &out[..n];
                for (i, p) in parts.iter().enumerate() {
                    assert!(
                        p.first <= p.last
                            && p.first >= parsed.first_usable
                            && p.last <= parsed.last_usable,
                        "case {case}"
                    );
                    for q in &parts[i + 1..] {
                        assert!(p.last < q.first || q.last < p.first, "case {case}: overlap");
                    }
                }
            }
        }
        // The mutations really reached the semantic checks, not only the CRCs.
        assert!(
            headers > 500 && tables > 300,
            "{headers} headers, {tables} tables"
        );
    }

    /// Both copies of the header on any disk size: an accepted header keeps its structures
    /// out of the usable range, the two copies' agreement is symmetric, and every listed
    /// partition has a name and a block count (CLAUDE.md §72).
    #[test]
    fn mutated_copies_keep_the_layout_and_agree_symmetrically() {
        use crate::testing::{Rng, mutate_window};
        let none = Partition {
            index: 0,
            kind: Guid::default(),
            unique: Guid::default(),
            first: 0,
            last: 0,
            attributes: 0,
            name: [0; 36],
        };
        let array = entries(&[
            (1, 2048, 2999, "Origin system A"),
            (2, 3000, DISK - 34, "Origin \u{263A} data"),
        ]);
        let good_primary = header(1, DISK - 1, 2, &array);
        let good_backup = header(DISK - 1, 1, DISK - 33, &array);
        let mut rng = Rng::new(0x4750_5432);
        let (mut accepted, mut agreeing, mut tables) = (0u32, 0u32, 0u32);
        for case in 0..crate::testing::cases(6000) {
            let (mut primary, mut backup) = (good_primary.clone(), good_backup.clone());
            // Mutate the fields the checks read, then re-seal, so the layout checks are what
            // refuses the bad ones. Every fourth case is left unsealed.
            let h = if case % 2 == 0 {
                &mut primary
            } else {
                &mut backup
            };
            mutate_window(h, 8..92, &mut rng);
            if case % 4 != 3 {
                let size = u32_at(h, 12) as usize;
                if (92..=BS).contains(&size) {
                    h[16..20].fill(0);
                    let c = crc32(&h[..size]);
                    h[16..20].copy_from_slice(&c.to_le_bytes());
                }
            }
            let disk = match case % 8 {
                6 => rng.edge_u64(),
                7 => DISK + rng.below(4) as u64,
                _ => DISK,
            };
            let (p, b) = (
                parse_header(&primary, 1, disk),
                parse_header(&backup, DISK - 1, disk),
            );
            for copy in [&p, &b] {
                let Ok(h) = copy else { continue };
                accepted += 1;
                assert!(
                    h.first_usable > 1 && h.first_usable <= h.last_usable,
                    "case {case}"
                );
                assert!(h.last_usable < disk - 1, "case {case}: {h:?}");
                for lba in [h.my_lba, h.alternate_lba] {
                    assert!(lba < disk, "case {case}: {h:?}");
                    assert!(
                        lba < h.first_usable || lba > h.last_usable,
                        "case {case}: a header at {lba} inside the usable range: {h:?}"
                    );
                }
                assert_ne!(h.my_lba, h.alternate_lba, "case {case}");
                let (end, blocks) = (
                    h.entries_lba + h.array_blocks(BS as u64),
                    h.array_blocks(BS as u64),
                );
                assert!(
                    blocks > 0 && end <= disk && h.entries_lba >= 1,
                    "case {case}: {h:?}"
                );
                assert!(
                    end <= h.first_usable || h.entries_lba > h.last_usable,
                    "case {case}"
                );
                for lba in [h.my_lba, h.alternate_lba] {
                    assert!(!(h.entries_lba..end).contains(&lba), "case {case}: {h:?}");
                }
                assert!(h.array_len() <= MAX_ARRAY, "case {case}");
                // Every accepted header's walk would CRC 16 KiB; every eighth is plenty.
                if case % 8 != 0 {
                    continue;
                }
                let mut out = [none; 8];
                if let Ok(n) = parse_entries(&array, h, &mut out) {
                    tables += 1;
                    for (i, q) in out[..n].iter().enumerate() {
                        assert!(i == 0 || out[i - 1].index < q.index, "case {case}");
                        assert!(!q.kind.is_zero(), "case {case}");
                        assert_eq!(q.blocks(), q.last - q.first + 1, "case {case}");
                        let _ = format!("{} {} {}", q.kind, q.unique, q.name());
                    }
                }
            }
            if let (Ok(p), Ok(b)) = (p, b) {
                assert_eq!(p.agrees_with(&b), b.agrees_with(&p), "case {case}");
                agreeing += u32::from(p.agrees_with(&b));
            }
        }
        assert!(
            accepted > 2000 && agreeing > 100 && tables > 100,
            "{accepted} accepted, {agreeing} agreeing, {tables} tables"
        );
    }

    /// A header at a block inside the usable range describes a disk whose partitions cover
    /// it: the alternate header of a primary, or the primary of a backup.
    #[test]
    fn a_header_inside_the_usable_range_is_refused() {
        let array = entries(&[(1, 2048, 2999, "a")]);
        let inside = header(1, DISK - 1, 2, &array);
        let mut h = inside.clone();
        h[32..40].copy_from_slice(&100u64.to_le_bytes());
        seal(&mut h);
        assert_eq!(
            parse_header(&h, 1, DISK),
            Err(GptError::Layout),
            "alternate at 100"
        );
        // The usable range may end right before the alternate header, and it may start
        // right after the primary array.
        let edge = header(1, DISK - 1, 2, &array);
        assert!(parse_header(&edge, 1, DISK).is_ok());
    }

    /// A hand-built header cannot make the walk panic with a zero or short entry size.
    #[test]
    fn a_short_entry_size_is_refused_not_panicked() {
        let array = entries(&[(1, 2048, 2999, "a")]);
        let h = parse_header(&header(1, DISK - 1, 2, &array), 1, DISK).unwrap();
        let mut out = [Partition {
            index: 0,
            kind: Guid::default(),
            unique: Guid::default(),
            first: 0,
            last: 0,
            attributes: 0,
            name: [0; 36],
        }; 2];
        for size in [0, 1, 96, 127] {
            let hand = Header {
                entry_size: size,
                ..h
            };
            assert_eq!(
                parse_entries(&array, &hand, &mut out),
                Err(GptError::Entries)
            );
        }
    }

    #[test]
    fn entry_checks() {
        let none = Partition {
            index: 0,
            kind: Guid::default(),
            unique: Guid::default(),
            first: 0,
            last: 0,
            attributes: 0,
            name: [0; 36],
        };
        let check = |parts: &[(u8, u64, u64, &str)]| {
            let array = entries(parts);
            let h = parse_header(&header(1, DISK - 1, 2, &array), 1, DISK).unwrap();
            let mut out = [none; 4];
            parse_entries(&array, &h, &mut out)
        };
        assert_eq!(
            check(&[(1, 2048, 2047, "")]),
            Err(GptError::Partition(0)),
            "ends before it starts"
        );
        assert_eq!(
            check(&[(1, 33, 100, "")]),
            Err(GptError::Partition(0)),
            "before the usable range"
        );
        assert_eq!(
            check(&[(1, 2048, DISK - 33, "")]),
            Err(GptError::Partition(0)),
            "after it"
        );
        assert_eq!(
            check(&[(1, 100, 200, ""), (2, 200, 300, "")]),
            Err(GptError::Overlap(0, 1))
        );
        assert_eq!(check(&[(1, 100, 200, ""), (2, 201, 300, "")]), Ok(2));
        assert_eq!(
            check(&[
                (1, 100, 100, ""),
                (2, 101, 101, ""),
                (3, 102, 102, ""),
                (4, 103, 103, ""),
                (5, 104, 104, "")
            ]),
            Err(GptError::Entries),
            "more than out holds"
        );
        // A flipped bit anywhere in the array.
        let array = entries(&[(1, 2048, 2999, "a")]);
        let h = parse_header(&header(1, DISK - 1, 2, &array), 1, DISK).unwrap();
        let mut bad = array.clone();
        bad[5000] ^= 1;
        assert_eq!(
            parse_entries(&bad, &h, &mut [none; 4]),
            Err(GptError::ArrayCrc)
        );
        assert_eq!(
            parse_entries(&array[..100], &h, &mut [none; 4]),
            Err(GptError::Entries),
            "short"
        );
    }
}
