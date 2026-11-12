//! **TEST ONLY, never shipped.** The pure half of a UEFI application that gives the boot test
//! firmware tables with reserved memory in them (TASKS item 40; research ledger
//! `docs/research/2026-11-12-reserved-memory-identity-maps.md` §4).
//!
//! QEMU 11.1 builds its DMAR (VT-d) and IVRS (AMD-Vi) with no RMRR and no IVMD, and appends the
//! tables `-acpitable` gives after its own, so the kernel, which reads the first valid table of
//! a signature, never sees one with reserved memory. The application runs before Origin's
//! loader: it allocates each region in pages the firmware's memory map then reports as
//! reserved, copies QEMU's table with the structures below appended into ACPI memory, points
//! the XSDT's entry at the copy, and starts the unchanged loader. The kernel then meets the
//! table where any firmware would put it.
//!
//! This half holds the configuration format (written by `cargo xtask`, read by the
//! application), the structures it appends, written from VT-d Rev 5.20 §8.4 and AMD 48882
//! Rev 3.11 §5.2.2.2 independently of the kernel's parsers, and the table edits. Pure, no
//! `unsafe`; host-tested, and the result is parsed back by the kernel's own `dmar::parse` and
//! `ivrs::parse` in the tests.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// The configuration file's first four bytes.
pub const MAGIC: [u8; 4] = *b"ORRV";
/// The configuration format's version.
pub const VERSION: u16 = 1;
/// Bytes of the header: magic, version, region count.
pub const HEADER: usize = 8;
/// Bytes of one region record.
pub const RECORD: usize = 24;
/// Regions a configuration may hold.
pub const MAX_REGIONS: usize = 16;
/// Bytes of one appended structure (an RMRR with one endpoint scope, or an IVMD).
pub const STRUCTURE: usize = 32;
/// A 4 KiB page.
pub const PAGE: u64 = 4096;

/// Which firmware table a region goes into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    /// An RMRR in the DMAR (VT-d).
    Dmar,
    /// An IVMD in the IVRS (AMD-Vi).
    Ivrs,
}

/// Where a region's memory comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Fresh pages of type `EfiReservedMemoryType`, filled with [`fill_byte`]: what a firmware
    /// that follows VT-d §8.4 reports.
    Reserved,
    /// Fresh pages of type `EfiLoaderData`, which the kernel takes back as its own RAM: a
    /// range the kernel must refuse.
    LoaderData,
    /// This physical address, allocated nowhere (for example the interrupt range).
    Fixed(u64),
}

/// One region the application plants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub table: Table,
    /// The IVMD type (20h, 21h or 22h); 0 for an RMRR.
    pub ivmd_kind: u8,
    /// The IVMD flags (48882 Table 113); 0 for an RMRR.
    pub flags: u8,
    pub placement: Placement,
    /// The device: an RMRR's endpoint on bus 0 or an IVMD's DeviceID, as bus << 8 | device << 3
    /// | function.
    pub device: u16,
    /// The last DeviceID of an IVMD range (22h).
    pub last_device: u16,
    /// Its length in 4 KiB pages (1 or more).
    pub pages: u32,
}

/// Why a configuration file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Not this format, or not this version.
    Header,
    /// The file's length does not match its region count, or the count is above
    /// [`MAX_REGIONS`].
    Length,
    /// The record at this index has an unknown table, placement or IVMD type, or no pages.
    Record(usize),
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let mut v = [0u8; 4];
    v.copy_from_slice(b.get(at..at + 4)?);
    Some(u32::from_le_bytes(v))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    let mut v = [0u8; 8];
    v.copy_from_slice(b.get(at..at + 8)?);
    Some(u64::from_le_bytes(v))
}

/// A configuration file for `regions` (at most [`MAX_REGIONS`]): the header, then one record
/// each.
pub fn encode(regions: &[Region], out: &mut [u8]) -> Option<usize> {
    if regions.len() > MAX_REGIONS {
        return None;
    }
    let len = HEADER + RECORD * regions.len();
    let out = out.get_mut(..len)?;
    out.fill(0);
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&(regions.len() as u16).to_le_bytes());
    for (r, rec) in regions.iter().zip(out[HEADER..].chunks_mut(RECORD)) {
        rec[0] = match r.table {
            Table::Dmar => 1,
            Table::Ivrs => 2,
        };
        rec[1] = r.ivmd_kind;
        rec[2] = r.flags;
        let (placement, fixed) = match r.placement {
            Placement::Reserved => (1, 0),
            Placement::LoaderData => (2, 0),
            Placement::Fixed(at) => (3, at),
        };
        rec[3] = placement;
        rec[4..6].copy_from_slice(&r.device.to_le_bytes());
        rec[6..8].copy_from_slice(&r.last_device.to_le_bytes());
        rec[8..12].copy_from_slice(&r.pages.to_le_bytes());
        rec[16..24].copy_from_slice(&fixed.to_le_bytes());
    }
    Some(len)
}

/// Reads a configuration file into `out`; returns how many regions it holds.
pub fn decode(b: &[u8], out: &mut [Region; MAX_REGIONS]) -> Result<usize, ConfigError> {
    if b.get(..4) != Some(&MAGIC[..]) || u16_at(b, 4) != Some(VERSION) {
        return Err(ConfigError::Header);
    }
    let count = usize::from(u16_at(b, 6).ok_or(ConfigError::Header)?);
    if count > MAX_REGIONS || b.len() != HEADER + RECORD * count {
        return Err(ConfigError::Length);
    }
    for (i, slot) in out.iter_mut().enumerate().take(count) {
        let bad = ConfigError::Record(i);
        let rec = b
            .get(HEADER + RECORD * i..HEADER + RECORD * (i + 1))
            .ok_or(bad)?;
        let table = match rec[0] {
            1 => Table::Dmar,
            2 => Table::Ivrs,
            _ => return Err(bad),
        };
        let placement = match rec[3] {
            1 => Placement::Reserved,
            2 => Placement::LoaderData,
            3 => Placement::Fixed(u64_at(rec, 16).ok_or(bad)?),
            _ => return Err(bad),
        };
        let pages = u32_at(rec, 8).ok_or(bad)?;
        let ivmd_kind = rec[1];
        if pages == 0 || (table == Table::Ivrs && !(0x20..=0x22).contains(&ivmd_kind)) {
            return Err(bad);
        }
        *slot = Region {
            table,
            ivmd_kind,
            flags: rec[2],
            placement,
            device: u16_at(rec, 4).ok_or(bad)?,
            last_device: u16_at(rec, 6).ok_or(bad)?,
            pages,
        };
    }
    Ok(count)
}

/// The byte a reserved region holds at `offset` from its start, `index` being the region's
/// place in the configuration: never zero, and different from page to page, so a copy that
/// lands one page off is seen.
pub fn fill_byte(index: usize, offset: u64) -> u8 {
    let page = offset / PAGE;
    let v = (index as u64)
        .wrapping_mul(0x9E37)
        .wrapping_add(page.wrapping_mul(0x51))
        .wrapping_add(offset % PAGE)
        % 255;
    v as u8 + 1
}

/// An RMRR (VT-d §8.4) over `[base, base + len)` in `segment`, naming one PCI endpoint on bus
/// `device >> 8` at device and function `device & 0xFF` by a one-step path (§8.3.1).
pub fn rmrr(segment: u16, base: u64, len: u64, device: u16) -> [u8; STRUCTURE] {
    let mut s = [0u8; STRUCTURE];
    s[0..2].copy_from_slice(&1u16.to_le_bytes()); // type 1
    s[2..4].copy_from_slice(&(STRUCTURE as u16).to_le_bytes()); // 24 + one 8-byte scope
    s[6..8].copy_from_slice(&segment.to_le_bytes());
    s[8..16].copy_from_slice(&base.to_le_bytes());
    s[16..24].copy_from_slice(&(base + len - 1).to_le_bytes()); // the last byte
    // Device scope: type 1 (endpoint), length 8, enumeration ID 0, start bus, path.
    s[24] = 1;
    s[25] = 8;
    s[28] = 0;
    s[29] = (device >> 8) as u8;
    s[30] = ((device >> 3) & 0x1F) as u8;
    s[31] = (device & 7) as u8;
    s
}

/// An IVMD (48882 Tables 111 and 112) of `kind` with `flags`, for DeviceID `device` (to
/// `last` for a range), over `[start, start + len)`.
pub fn ivmd(
    kind: u8,
    flags: u8,
    device: u16,
    last: u16,
    segment: u16,
    start: u64,
    len: u64,
) -> [u8; STRUCTURE] {
    let mut s = [0u8; STRUCTURE];
    s[0] = kind;
    s[1] = flags;
    s[2..4].copy_from_slice(&(STRUCTURE as u16).to_le_bytes());
    if kind != 0x20 {
        s[4..6].copy_from_slice(&device.to_le_bytes());
        s[8..10].copy_from_slice(&segment.to_le_bytes());
    }
    if kind == 0x22 {
        s[6..8].copy_from_slice(&last.to_le_bytes());
    }
    s[16..24].copy_from_slice(&start.to_le_bytes());
    s[24..32].copy_from_slice(&len.to_le_bytes());
    s
}

/// The structure `r` asks for, over `[base, base + PAGE * pages)`.
pub fn structure(r: &Region, base: u64) -> [u8; STRUCTURE] {
    let len = u64::from(r.pages) * PAGE;
    match r.table {
        Table::Dmar => rmrr(0, base, len, r.device),
        Table::Ivrs => ivmd(r.ivmd_kind, r.flags, r.device, r.last_device, 0, base, len),
    }
}

/// Sets byte 9 of an ACPI table so its bytes sum to zero (ACPI 6.6 §5.2.6).
pub fn fix_checksum(table: &mut [u8]) {
    if table.len() < 10 {
        return;
    }
    table[9] = 0;
    let sum = table.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    table[9] = 0u8.wrapping_sub(sum);
}

/// Writes `old` (a whole ACPI table, its length field consistent) with `extra` appended into
/// `out`, with the length and checksum fixed; returns the new length.
pub fn grow(old: &[u8], extra: &[u8], out: &mut [u8]) -> Option<usize> {
    let declared = usize::try_from(u32_at(old, 4)?).ok()?;
    if declared != old.len() || old.len() < 36 {
        return None;
    }
    let len = old.len().checked_add(extra.len())?;
    let out = out.get_mut(..len)?;
    out[..old.len()].copy_from_slice(old);
    out[old.len()..].copy_from_slice(extra);
    out[4..8].copy_from_slice(&u32::try_from(len).ok()?.to_le_bytes());
    fix_checksum(out);
    Some(len)
}

/// The XSDT's entries (ACPI 6.6 §5.2.8): 8-byte physical addresses after the 36-byte header.
pub fn xsdt_entries(xsdt: &[u8]) -> impl Iterator<Item = (usize, u64)> + '_ {
    let len = u32_at(xsdt, 4).map_or(0, |l| l as usize).min(xsdt.len());
    let body = xsdt.get(36..len).unwrap_or(&[]);
    body.as_chunks::<8>()
        .0
        .iter()
        .enumerate()
        .map(|(i, c)| (i, u64::from_le_bytes(*c)))
}

/// Points XSDT entry `index` at `addr` and fixes the XSDT's checksum.
pub fn xsdt_set(xsdt: &mut [u8], index: usize, addr: u64) -> Option<()> {
    let at = 36 + 8 * index;
    let len = usize::try_from(u32_at(xsdt, 4)?).ok()?;
    if at + 8 > len || len > xsdt.len() {
        return None;
    }
    xsdt[at..at + 8].copy_from_slice(&addr.to_le_bytes());
    fix_checksum(&mut xsdt[..len]);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(b: &[u8]) -> u8 {
        b.iter().fold(0u8, |s, &x| s.wrapping_add(x))
    }

    fn regions() -> [Region; 3] {
        [
            Region {
                table: Table::Dmar,
                ivmd_kind: 0,
                flags: 0,
                placement: Placement::Reserved,
                device: 0x0010,
                last_device: 0,
                pages: 2,
            },
            Region {
                table: Table::Ivrs,
                ivmd_kind: 0x22,
                flags: 0b0011,
                placement: Placement::Fixed(0xFEE0_0000),
                device: 0x0010,
                last_device: 0x0018,
                pages: 1,
            },
            Region {
                table: Table::Ivrs,
                ivmd_kind: 0x20,
                flags: 0b1000,
                placement: Placement::LoaderData,
                device: 0,
                last_device: 0,
                pages: 3,
            },
        ]
    }

    #[test]
    fn a_configuration_round_trips() {
        let mut buf = [0u8; 256];
        let len = encode(&regions(), &mut buf).unwrap();
        assert_eq!(len, HEADER + 3 * RECORD);
        let mut out = [regions()[0]; MAX_REGIONS];
        assert_eq!(decode(&buf[..len], &mut out), Ok(3));
        assert_eq!(&out[..3], &regions()[..]);
    }

    #[test]
    fn a_bad_configuration_is_refused() {
        let mut buf = [0u8; 256];
        let len = encode(&regions(), &mut buf).unwrap();
        let mut out = [regions()[0]; MAX_REGIONS];
        assert_eq!(decode(&buf[..len - 1], &mut out), Err(ConfigError::Length));
        let mut bad = buf;
        bad[0] = b'X';
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Header));
        let mut bad = buf;
        bad[HEADER + RECORD] = 9; // table
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Record(1)));
        let mut bad = buf;
        bad[HEADER + RECORD + 1] = 0x23; // IVMD type
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Record(1)));
        let mut bad = buf;
        bad[HEADER + 2 * RECORD + 8] = 0; // pages
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Record(2)));
        let mut bad = buf;
        bad[HEADER + 3] = 4; // placement
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Record(0)));
        assert_eq!(
            encode(&[regions()[0]; MAX_REGIONS + 1], &mut [0u8; 1024]),
            None
        );
    }

    #[test]
    fn the_fill_is_never_zero_and_differs_between_pages() {
        for index in 0..4 {
            for offset in (0..4 * PAGE).step_by(97) {
                assert_ne!(fill_byte(index, offset), 0);
            }
            assert_ne!(fill_byte(index, 0), fill_byte(index, PAGE));
        }
    }

    #[test]
    fn grow_appends_and_fixes_length_and_checksum() {
        let mut old = [0u8; 40];
        old[..4].copy_from_slice(b"TEST");
        old[4..8].copy_from_slice(&40u32.to_le_bytes());
        fix_checksum(&mut old);
        assert_eq!(sum(&old), 0);
        let mut out = [0u8; 128];
        let len = grow(&old, &[1, 2, 3], &mut out).unwrap();
        assert_eq!(len, 43);
        assert_eq!(u32_at(&out, 4), Some(43));
        assert_eq!(&out[40..43], &[1, 2, 3]);
        assert_eq!(sum(&out[..len]), 0);
        // A length field that disagrees, or no room.
        let mut lying = old;
        lying[4] = 41;
        assert_eq!(grow(&lying, &[], &mut out), None);
        assert_eq!(grow(&old, &[0; 100], &mut out), None);
    }

    #[test]
    fn xsdt_entries_are_read_and_set() {
        let mut x = [0u8; 36 + 24];
        x[..4].copy_from_slice(b"XSDT");
        x[4..8].copy_from_slice(&60u32.to_le_bytes());
        for (i, a) in [0x1000u64, 0x2000, 0x3000].iter().enumerate() {
            x[36 + 8 * i..44 + 8 * i].copy_from_slice(&a.to_le_bytes());
        }
        fix_checksum(&mut x);
        let entries: Vec<_> = xsdt_entries(&x).collect();
        assert_eq!(entries, [(0, 0x1000), (1, 0x2000), (2, 0x3000)]);
        xsdt_set(&mut x, 1, 0xABC000).unwrap();
        assert_eq!(xsdt_entries(&x).nth(1), Some((1, 0xABC000)));
        assert_eq!(sum(&x), 0);
        assert_eq!(xsdt_set(&mut x, 3, 0), None);
    }

    /// A DMAR as QEMU 11.1 builds it for q35 (one DRHD, endpoints 00:02.0 and 00:03.0), with
    /// an RMRR for 00:02.0 appended, is accepted by the kernel's own parser, which finds the
    /// range and its one device.
    #[test]
    fn the_kernel_reads_the_planted_rmrr() {
        use origin_frame::dmar::{self, BridgeBuses, Requester};
        struct NoBridges;
        impl BridgeBuses for NoBridges {
            fn buses(&mut self, _: Requester) -> Option<(u8, u8)> {
                None
            }
        }
        let mut old = vec![0u8; 48];
        old[..4].copy_from_slice(b"DMAR");
        old[8] = 1;
        old[36] = 47; // host address width 48
        old[37] = 1; // interrupt remapping
        let mut drhd = vec![0u8, 0, 0, 0, 0, 0, 0, 0];
        drhd.extend_from_slice(&0xFED9_0000u64.to_le_bytes());
        for dev in [2u8, 3] {
            drhd.extend_from_slice(&[1, 8, 0, 0, 0, 0, dev, 0]);
        }
        let n = drhd.len() as u16;
        drhd[2..4].copy_from_slice(&n.to_le_bytes());
        old.extend_from_slice(&drhd);
        let n = old.len() as u32;
        old[4..8].copy_from_slice(&n.to_le_bytes());
        fix_checksum(&mut old);
        let region = regions()[0];
        let rmrr = structure(&region, 0x7A00_0000);
        let mut out = vec![0u8; 512];
        let len = grow(&old, &rmrr, &mut out).unwrap();
        let table = &out[..len];
        assert_eq!(sum(table), 0);
        let d = dmar::parse(table).unwrap();
        let r: Vec<_> = d.reserved_memory().collect();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].base, r[0].limit), (0x7A00_0000, 0x7A00_1FFF));
        let at = |device| Requester {
            segment: 0,
            bus: 0,
            device,
            function: 0,
        };
        assert!(r[0].covers(at(2), &mut NoBridges));
        assert!(!r[0].covers(at(3), &mut NoBridges));
        assert_eq!(d.units().count(), 1);
    }

    /// An IVRS like QEMU's (one IOMMU, a type 10h block selecting 00:02.0 and 00:03.0), with an
    /// IVMD of each type appended, is accepted by the kernel's own parser, which places them
    /// with the IOMMU before them.
    #[test]
    fn the_kernel_reads_the_planted_ivmds() {
        use origin_frame::ivrs;
        let mut old = vec![0u8; 48];
        old[..4].copy_from_slice(b"IVRS");
        old[8] = 1;
        let mut ivhd = vec![0x10u8, 0, 0, 0, 0x28, 0, 0x40, 0];
        ivhd.extend_from_slice(&0xFED8_0000u64.to_le_bytes());
        ivhd.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // segment, info, feature
        for id in [0x10u16, 0x18] {
            let b = id.to_le_bytes();
            ivhd.extend_from_slice(&[2, b[0], b[1], 0]);
        }
        let n = ivhd.len() as u16;
        ivhd[2..4].copy_from_slice(&n.to_le_bytes());
        old.extend_from_slice(&ivhd);
        let n = old.len() as u32;
        old[4..8].copy_from_slice(&n.to_le_bytes());
        fix_checksum(&mut old);
        let mut extra = Vec::new();
        for (i, r) in regions().iter().enumerate().skip(1) {
            extra.extend_from_slice(&structure(r, 0x7B00_0000 + 0x1_0000 * i as u64));
        }
        let mut out = vec![0u8; 512];
        let len = grow(&old, &extra, &mut out).unwrap();
        let t = ivrs::parse(&out[..len]).unwrap();
        let m: Vec<_> = t.memory().collect();
        assert_eq!(m.len(), 2);
        assert_eq!(
            (m[0].kind, m[0].flags, m[0].start),
            (0x22, 0b0011, 0x7B01_0000)
        );
        assert_eq!((m[0].device_id, m[0].last_device_id), (0x10, 0x18));
        assert_eq!(
            (m[1].kind, m[1].flags, m[1].length),
            (0x20, 0b1000, 3 * PAGE)
        );
        assert!(m.iter().all(|m| m.unit_base == 0xFED8_0000));
        assert_eq!(t.memory_for(0xFED8_0000, 0, 0x18, 0x18).count(), 2);
    }

    #[test]
    fn an_rmrr_names_its_endpoint_and_inclusive_limit() {
        let s = rmrr(0, 0x7A00_0000, 2 * PAGE, 0x0010);
        assert_eq!(u16_at(&s, 0), Some(1));
        assert_eq!(u16_at(&s, 2), Some(32));
        assert_eq!(u64_at(&s, 8), Some(0x7A00_0000));
        assert_eq!(u64_at(&s, 16), Some(0x7A00_1FFF));
        assert_eq!(&s[24..32], &[1, 8, 0, 0, 0, 0, 2, 0]);
    }

    #[test]
    fn an_ivmd_carries_only_its_types_fields() {
        let all = ivmd(0x20, 0b1000, 0x10, 0x18, 3, 0x1000, PAGE);
        assert_eq!(&all[..8], &[0x20, 0b1000, 32, 0, 0, 0, 0, 0]);
        assert_eq!(u16_at(&all, 8), Some(0));
        let one = ivmd(0x21, 0b0111, 0x10, 0x18, 0, 0x1000, PAGE);
        assert_eq!(&one[..8], &[0x21, 0b0111, 32, 0, 0x10, 0, 0, 0]);
        let range = ivmd(0x22, 0b0011, 0x10, 0x18, 0, 0x1000, PAGE);
        assert_eq!(&range[..8], &[0x22, 0b0011, 32, 0, 0x10, 0, 0x18, 0]);
        assert_eq!(u64_at(&range, 16), Some(0x1000));
        assert_eq!(u64_at(&range, 24), Some(PAGE));
    }
}
