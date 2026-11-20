//! **TEST ONLY, never shipped.** The pure half of a UEFI application that gives the boot test
//! firmware tables with reserved memory in them (TASKS item 40; research ledger
//! `docs/research/2026-11-12-reserved-memory-identity-maps.md` §4), and the firmware-table shapes
//! of the board rental B ran on (TASKS item 83; `docs/research/2026-11-20-rental-b-findings.md`):
//! an RSDP and XSDT in ACPI NVS memory, a FACS named only by the FADT's 32-bit field, an FPDT
//! whose FBPT and S3PT lie in reserved memory, and reserved pages at the top of the RAM. The
//! shapes are built from that ledger's description and ACPI 6.6, never from the rental's tables.
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
/// The configuration format's version (2: [`Placement::ReservedOther`] and [`Placement::Table`],
/// and a fill bound to each page's address; 3: the [`Shape`] in the header, TASKS item 83).
pub const VERSION: u16 = 3;
/// Bytes of the header: magic, version, region count, the shape's flags and its reserved pages
/// at the top of the RAM.
pub const HEADER: usize = 16;
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
    /// Fresh pages of type `EfiReservedMemoryType` that hold something other than the fill
    /// (each page holds the fill of the page 1 MiB above it), as a real firmware's own memory
    /// would: the kernel's proof must not write into them (TASKS item 40's review, M2).
    ReservedOther,
    /// The page that holds the application's copy of the table itself, which it puts in
    /// `EfiReservedMemoryType` pages: a writable range there covers a table the kernel holds as
    /// a reference, and the kernel must refuse it (TASKS item 40's review, M1).
    Table,
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

/// What the application shapes besides the regions: rental B's board's firmware tables (TASKS
/// item 83). [`Shape::default`] shapes nothing, as for `boot-reserved`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    /// The RSDP and the XSDT copied into `EfiACPIMemoryNVS` pages, and the configuration
    /// table's ACPI 2.0 entry pointed at the RSDP's copy.
    pub acpi_nvs: bool,
    /// The FADT copied with `X_FIRMWARE_CTRL` zero, so the FACS is named only by the 32-bit
    /// `FIRMWARE_CTRL` ([`facs_by_32_bit_field`]).
    pub facs_32: bool,
    /// An FPDT added to the XSDT, its FBPT and S3PT in `EfiReservedMemoryType` pages.
    pub fpdt: bool,
    /// `EfiReservedMemoryType` pages taken at the top of the RAM, as the board's firmware keeps
    /// a range there; 0 for none.
    pub top_reserved_pages: u32,
}

impl Shape {
    /// Whether it shapes anything.
    pub const fn any(&self) -> bool {
        self.acpi_nvs || self.facs_32 || self.fpdt || self.top_reserved_pages != 0
    }

    const fn flags(&self) -> u32 {
        (self.acpi_nvs as u32) | (self.facs_32 as u32) << 1 | (self.fpdt as u32) << 2
    }
}

/// The flags a [`Shape`] may set.
const SHAPE_FLAGS: u32 = 0b111;

/// Why a configuration file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Not this format, or not this version, or a shape flag this version does not define.
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

/// A configuration file for `regions` (at most [`MAX_REGIONS`]) and `shape`: the header, then
/// one record each.
pub fn encode(regions: &[Region], shape: Shape, out: &mut [u8]) -> Option<usize> {
    if regions.len() > MAX_REGIONS {
        return None;
    }
    let len = HEADER + RECORD * regions.len();
    let out = out.get_mut(..len)?;
    out.fill(0);
    out[..4].copy_from_slice(&MAGIC);
    out[4..6].copy_from_slice(&VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&(regions.len() as u16).to_le_bytes());
    out[8..12].copy_from_slice(&shape.flags().to_le_bytes());
    out[12..16].copy_from_slice(&shape.top_reserved_pages.to_le_bytes());
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
            Placement::ReservedOther => (4, 0),
            Placement::Table => (5, 0),
        };
        rec[3] = placement;
        rec[4..6].copy_from_slice(&r.device.to_le_bytes());
        rec[6..8].copy_from_slice(&r.last_device.to_le_bytes());
        rec[8..12].copy_from_slice(&r.pages.to_le_bytes());
        rec[16..24].copy_from_slice(&fixed.to_le_bytes());
    }
    Some(len)
}

/// Reads a configuration file into `out`; returns how many regions it holds, and its shape.
pub fn decode(b: &[u8], out: &mut [Region; MAX_REGIONS]) -> Result<(usize, Shape), ConfigError> {
    if b.get(..4) != Some(&MAGIC[..]) || u16_at(b, 4) != Some(VERSION) {
        return Err(ConfigError::Header);
    }
    let count = usize::from(u16_at(b, 6).ok_or(ConfigError::Header)?);
    let flags = u32_at(b, 8).ok_or(ConfigError::Header)?;
    if flags & !SHAPE_FLAGS != 0 {
        return Err(ConfigError::Header);
    }
    let shape = Shape {
        acpi_nvs: flags & 1 != 0,
        facs_32: flags & 2 != 0,
        fpdt: flags & 4 != 0,
        top_reserved_pages: u32_at(b, 12).ok_or(ConfigError::Header)?,
    };
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
            4 => Placement::ReservedOther,
            5 => Placement::Table,
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
    Ok((count, shape))
}

/// The first 16 bytes of every page the application fills.
pub const FILL_MAGIC: [u8; 16] = *b"ORIGIN TEST FILL";

/// The byte the application puts at `offset` (below [`PAGE`]) of the reserved page at physical
/// address `page`: [`FILL_MAGIC`], then the page's own address, then bytes that are never zero
/// and differ from page to page, so a copy that lands on another page is seen. The kernel
/// recognises a page by it (`framekernel::testfill`, which `xtask`'s tests hold equal to this)
/// and writes into no reserved page that lacks it (TASKS item 40's review, M2).
pub fn fill_byte(page: u64, offset: u64) -> u8 {
    match offset {
        0..16 => FILL_MAGIC[offset as usize],
        16..24 => page.to_le_bytes()[(offset - 16) as usize],
        _ => {
            let v = (page >> 12).wrapping_mul(0x9E37).wrapping_add(offset) % 255;
            v as u8 + 1
        }
    }
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

/// Bytes of an ACPI 2.0 RSDP (ACPI 6.6 §5.2.5.3).
pub const RSDP_LEN: usize = 36;
/// Bytes of the FPDT the application adds: the 36-byte header and two 16-byte pointer records
/// (§5.2.24, Tables 5.121, 5.125 and 5.126).
pub const FPDT_LEN: usize = 68;
/// Bytes of its FBPT: the 8-byte header and the 48-byte Host Firmware Boot Performance Data
/// Record (§5.2.24.8 and §5.2.24.9, Tables 5.129 and 5.130).
pub const FBPT_LEN: usize = 56;
/// Bytes of its S3PT: the 8-byte header and the 24-byte Basic S3 Resume Performance Record
/// (§5.2.24.10, Tables 5.131 and 5.132).
pub const S3PT_LEN: usize = 32;
/// The FADT's 32-bit FACS field, and its 64-bit one (§5.2.9).
const FIRMWARE_CTRL: usize = 36;
const X_FIRMWARE_CTRL: usize = 132;

/// A copy of the ACPI 2.0 RSDP `old` that names the XSDT at `xsdt`, with both checksums fixed:
/// the first 20 bytes', and the whole structure's (§5.2.5.3). `None` unless `old` is an RSDP of
/// revision 2 or later.
pub fn rsdp_naming(old: &[u8], xsdt: u64) -> Option<[u8; RSDP_LEN]> {
    let old = old.get(..RSDP_LEN)?;
    if &old[..8] != b"RSD PTR " || old[15] < 2 || u32_at(old, 20)? as usize != RSDP_LEN {
        return None;
    }
    let mut new = [0u8; RSDP_LEN];
    new.copy_from_slice(old);
    new[24..32].copy_from_slice(&xsdt.to_le_bytes());
    new[8] = 0;
    new[8] = 0u8.wrapping_sub(new[..20].iter().fold(0u8, |s, &b| s.wrapping_add(b)));
    new[32] = 0;
    new[32] = 0u8.wrapping_sub(new.iter().fold(0u8, |s, &b| s.wrapping_add(b)));
    Some(new)
}

/// Makes the FADT `fadt` name its FACS only by the 32-bit `FIRMWARE_CTRL`: the FACS the table
/// names (`X_FIRMWARE_CTRL` when non-zero, else `FIRMWARE_CTRL`, as §5.2.9 chooses) goes into
/// `FIRMWARE_CTRL`, `X_FIRMWARE_CTRL` becomes 0, and the checksum is fixed. Returns the FACS's
/// address; `None`, and nothing changed, unless the table is a FADT long enough to hold
/// `X_FIRMWARE_CTRL` whose FACS is named and lies below 4 GiB.
pub fn facs_by_32_bit_field(fadt: &mut [u8]) -> Option<u32> {
    let len = usize::try_from(u32_at(fadt, 4)?).ok()?;
    if fadt.get(..4) != Some(&b"FACP"[..]) || len > fadt.len() || len < X_FIRMWARE_CTRL + 8 {
        return None;
    }
    let x = u64_at(fadt, X_FIRMWARE_CTRL)?;
    let facs = if x != 0 {
        u32::try_from(x).ok()?
    } else {
        u32_at(fadt, FIRMWARE_CTRL)?
    };
    if facs == 0 {
        return None;
    }
    fadt[FIRMWARE_CTRL..FIRMWARE_CTRL + 4].copy_from_slice(&facs.to_le_bytes());
    fadt[X_FIRMWARE_CTRL..X_FIRMWARE_CTRL + 8].fill(0);
    fix_checksum(&mut fadt[..len]);
    Some(facs)
}

/// An FBPT (§5.2.24.8): its header and the Host Firmware Boot Performance Data Record, type 2,
/// revision 2, with every timer zero, as no boot was timed.
pub fn fbpt() -> [u8; FBPT_LEN] {
    let mut t = [0u8; FBPT_LEN];
    t[..4].copy_from_slice(b"FBPT");
    t[4..8].copy_from_slice(&(FBPT_LEN as u32).to_le_bytes());
    t[8..10].copy_from_slice(&2u16.to_le_bytes());
    t[10] = 48;
    t[11] = 2;
    t
}

/// An S3PT (§5.2.24.10): its header and the Basic S3 Resume Performance Record, type 0, revision
/// 1, with no resume counted.
pub fn s3pt() -> [u8; S3PT_LEN] {
    let mut t = [0u8; S3PT_LEN];
    t[..4].copy_from_slice(b"S3PT");
    t[4..8].copy_from_slice(&(S3PT_LEN as u32).to_le_bytes());
    t[8..10].copy_from_slice(&0u16.to_le_bytes());
    t[10] = 24;
    t[11] = 1;
    t
}

/// An FPDT, revision 1 (§5.2.24, Table 5.121), with a Host Firmware Boot Performance Table
/// Pointer Record naming `fbpt` and an S3 Performance Table Pointer Record naming `s3pt`
/// (Tables 5.125 and 5.126), and its checksum fixed. The OEM fields name this application.
pub fn fpdt(fbpt: u64, s3pt: u64) -> [u8; FPDT_LEN] {
    let mut t = [0u8; FPDT_LEN];
    t[..4].copy_from_slice(b"FPDT");
    t[4..8].copy_from_slice(&(FPDT_LEN as u32).to_le_bytes());
    t[8] = 1;
    t[10..16].copy_from_slice(b"ORIGIN");
    t[16..24].copy_from_slice(b"TESTFPDT");
    t[24..28].copy_from_slice(&1u32.to_le_bytes());
    t[28..32].copy_from_slice(b"ORGN");
    t[32..36].copy_from_slice(&1u32.to_le_bytes());
    for (at, kind, target) in [(36, 0u16, fbpt), (52, 1u16, s3pt)] {
        t[at..at + 2].copy_from_slice(&kind.to_le_bytes());
        t[at + 2] = 16;
        t[at + 3] = 1;
        t[at + 8..at + 16].copy_from_slice(&target.to_le_bytes());
    }
    fix_checksum(&mut t);
    t
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

    fn rented() -> Shape {
        Shape {
            acpi_nvs: true,
            facs_32: true,
            fpdt: true,
            top_reserved_pages: 8192,
        }
    }

    #[test]
    fn a_configuration_round_trips() {
        let mut buf = [0u8; 256];
        let len = encode(&regions(), Shape::default(), &mut buf).unwrap();
        assert_eq!(len, HEADER + 3 * RECORD);
        let mut out = [regions()[0]; MAX_REGIONS];
        assert_eq!(decode(&buf[..len], &mut out), Ok((3, Shape::default())));
        assert_eq!(&out[..3], &regions()[..]);
        // A shape, with no regions or with them.
        let len = encode(&[], rented(), &mut buf).unwrap();
        assert_eq!(len, HEADER);
        assert_eq!(decode(&buf[..len], &mut out), Ok((0, rented())));
        for one in [
            Shape {
                acpi_nvs: true,
                ..Shape::default()
            },
            Shape {
                facs_32: true,
                ..Shape::default()
            },
            Shape {
                fpdt: true,
                ..Shape::default()
            },
            Shape {
                top_reserved_pages: 1,
                ..Shape::default()
            },
        ] {
            let len = encode(&regions(), one, &mut buf).unwrap();
            assert_eq!(decode(&buf[..len], &mut out), Ok((3, one)));
            assert!(one.any());
        }
        assert!(!Shape::default().any());
    }

    #[test]
    fn a_bad_configuration_is_refused() {
        let mut buf = [0u8; 256];
        let len = encode(&regions(), Shape::default(), &mut buf).unwrap();
        let mut out = [regions()[0]; MAX_REGIONS];
        assert_eq!(decode(&buf[..len - 1], &mut out), Err(ConfigError::Length));
        let mut bad = buf;
        bad[8] = 0b1000; // a shape flag version 3 does not define
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Header));
        let mut bad = buf;
        bad[4] = 2; // version 2's files have an 8-byte header
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Header));
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
        bad[HEADER + 3] = 6; // placement
        assert_eq!(decode(&bad[..len], &mut out), Err(ConfigError::Record(0)));
        let mut other = buf;
        other[HEADER + 3] = 4; // the reserved pages without the fill
        assert_eq!(decode(&other[..len], &mut out), Ok((3, Shape::default())));
        assert_eq!(out[0].placement, Placement::ReservedOther);
        other[HEADER + 3] = 5; // the table's own page
        assert_eq!(decode(&other[..len], &mut out), Ok((3, Shape::default())));
        assert_eq!(out[0].placement, Placement::Table);
        assert_eq!(
            encode(
                &[regions()[0]; MAX_REGIONS + 1],
                Shape::default(),
                &mut [0u8; 1024]
            ),
            None
        );
    }

    #[test]
    fn the_fill_is_bound_to_its_page_and_never_zero_after_its_header() {
        for page in [0x1000, 0x1FB3_1000, 0xFF_FFFF_F000] {
            let fill: [u8; PAGE as usize] = core::array::from_fn(|i| fill_byte(page, i as u64));
            assert_eq!(&fill[..16], &FILL_MAGIC);
            assert_eq!(fill[16..24], page.to_le_bytes());
            assert!(fill[24..].iter().all(|&b| b != 0));
            let next: [u8; PAGE as usize] =
                core::array::from_fn(|i| fill_byte(page + 0x10_0000, i as u64));
            assert_ne!(fill[24..], next[24..]);
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

    // TASKS item 83: rental B's board's shapes.

    /// An ACPI table: `signature`, its length, revision `rev`, `body` after the header, checksum
    /// fixed.
    fn table(signature: &[u8; 4], rev: u8, body: &[u8]) -> Vec<u8> {
        let mut t = vec![0u8; 36];
        t[..4].copy_from_slice(signature);
        t[8] = rev;
        t[10..16].copy_from_slice(b"ORIGIN");
        t.extend_from_slice(body);
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        fix_checksum(&mut t);
        t
    }

    /// A FADT of revision 6 (276 bytes) naming `facs` by `X_FIRMWARE_CTRL` only, or by both
    /// fields, and the DSDT at `dsdt` by both of its fields.
    fn fadt(facs: u64, both: bool, dsdt: u32) -> Vec<u8> {
        let mut body = vec![0u8; 276 - 36];
        if both {
            body[FIRMWARE_CTRL - 36..FIRMWARE_CTRL - 32]
                .copy_from_slice(&(facs as u32).to_le_bytes());
        }
        body[40 - 36..44 - 36].copy_from_slice(&dsdt.to_le_bytes());
        body[X_FIRMWARE_CTRL - 36..X_FIRMWARE_CTRL - 28].copy_from_slice(&facs.to_le_bytes());
        body[140 - 36..148 - 36].copy_from_slice(&u64::from(dsdt).to_le_bytes());
        let mut t = table(b"FACP", 6, &body);
        t[131] = 0; // FADT minor version
        fix_checksum(&mut t);
        t
    }

    #[test]
    fn the_facs_ends_up_named_by_the_32_bit_field_alone() {
        for both in [false, true] {
            let mut f = fadt(0x7FBD_0000, both, 0x7FB0_0000);
            assert_eq!(facs_by_32_bit_field(&mut f), Some(0x7FBD_0000));
            assert_eq!(u32_at(&f, FIRMWARE_CTRL), Some(0x7FBD_0000));
            assert_eq!(u64_at(&f, X_FIRMWARE_CTRL), Some(0));
            assert_eq!(sum(&f), 0);
            // The DSDT's two fields are untouched.
            assert_eq!(u32_at(&f, 40), Some(0x7FB0_0000));
            assert_eq!(u64_at(&f, 140), Some(0x7FB0_0000));
        }
        // A FACS above 4 GiB cannot be named by 32 bits; nothing changes.
        let mut high = fadt(0x1_0000_0000, false, 0x7FB0_0000);
        let before = high.clone();
        assert_eq!(facs_by_32_bit_field(&mut high), None);
        assert_eq!(high, before);
        // No FACS, too short, or not a FADT: refused.
        assert_eq!(facs_by_32_bit_field(&mut fadt(0, false, 1)), None);
        assert_eq!(facs_by_32_bit_field(&mut table(b"FACP", 1, &[0; 80])), None);
        let mut other = fadt(0x7FBD_0000, true, 1);
        other[..4].copy_from_slice(b"APIC");
        assert_eq!(facs_by_32_bit_field(&mut other), None);
    }

    #[test]
    fn the_rsdp_copy_names_the_new_xsdt_with_both_checksums() {
        let mut old = [0u8; RSDP_LEN];
        old[..8].copy_from_slice(b"RSD PTR ");
        old[9..15].copy_from_slice(b"ORIGIN");
        old[15] = 2;
        old[16..20].copy_from_slice(&0x7FB7_E000u32.to_le_bytes());
        old[20..24].copy_from_slice(&36u32.to_le_bytes());
        old[24..32].copy_from_slice(&0x7FB7_D000u64.to_le_bytes());
        let new = rsdp_naming(&old, 0x8E4F_1000).unwrap();
        assert_eq!(u64_at(&new, 24), Some(0x8E4F_1000));
        assert_eq!(u32_at(&new, 16), Some(0x7FB7_E000), "the RSDT stays named");
        assert_eq!(sum(&new[..20]), 0);
        assert_eq!(sum(&new), 0);
        let mut v1 = old;
        v1[15] = 0;
        assert_eq!(rsdp_naming(&v1, 1), None, "ACPI 1.0's RSDP has no XSDT");
        assert_eq!(rsdp_naming(&old[..20], 1), None);
    }

    #[test]
    fn the_performance_tables_have_their_minimum_layouts() {
        let (b, s) = (fbpt(), s3pt());
        assert_eq!((&b[..4], u32_at(&b, 4)), (&b"FBPT"[..], Some(56)));
        assert_eq!((u16_at(&b, 8), b[10], b[11]), (Some(2), 48, 2));
        assert_eq!((&s[..4], u32_at(&s, 4)), (&b"S3PT"[..], Some(32)));
        assert_eq!((u16_at(&s, 8), s[10], s[11]), (Some(0), 24, 1));
        let f = fpdt(0x8E36_4000, 0x8E37_D000);
        assert_eq!((&f[..4], u32_at(&f, 4), f[8]), (&b"FPDT"[..], Some(68), 1));
        assert_eq!(sum(&f), 0);
        assert_eq!((u16_at(&f, 36), f[38], f[39]), (Some(0), 16, 1));
        assert_eq!(u64_at(&f, 44), Some(0x8E36_4000));
        assert_eq!((u16_at(&f, 52), f[54], f[55]), (Some(1), 16, 1));
        assert_eq!(u64_at(&f, 60), Some(0x8E37_D000));
    }

    /// The shapes as the application leaves them, in a pretend physical memory, read by the
    /// kernel's own locator and lookup (`origin_frame::acpi`), as `firmware::acpi_table` reads
    /// them at the console's move: the FACS (by the 32-bit field), the FBPT and the S3PT are
    /// located and recorded as never lent, and the SPCR the XSDT lists is found and lent.
    #[test]
    fn the_kernels_locator_reads_the_shaped_tables() {
        use origin_frame::acpi::{self, Changing};
        // Addresses as the board's map has them, in their kinds' ranges: the RSDP, XSDT and FACS
        // in ACPI NVS, the FADT, FPDT, DSDT and SPCR in ACPI data, the FBPT and S3PT in
        // reserved memory.
        const RSDP: u64 = 0x9046_6014;
        const XSDT: u64 = 0x9046_5728;
        const FACS: u64 = 0x9046_3000;
        const FADT: u64 = 0x8E47_3000;
        const DSDT: u64 = 0x8E3F_5000;
        const SPCR: u64 = 0x8E47_2000;
        const FPDT: u64 = 0x8E46_9000;
        const FBPT: u64 = 0x8E36_4000;
        const S3PT: u64 = 0x8E37_D000;
        let mut memory: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut f = fadt(FACS, false, DSDT as u32);
        facs_by_32_bit_field(&mut f).unwrap();
        memory.push((FADT, f));
        let mut facs = vec![0u8; 64];
        facs[..4].copy_from_slice(b"FACS");
        facs[4..8].copy_from_slice(&64u32.to_le_bytes());
        memory.push((FACS, facs));
        memory.push((DSDT, table(b"DSDT", 2, &[0x10; 4])));
        let mut spcr = vec![0u8; 80 - 36];
        spcr[0] = 0; // interface type 0, a full 16550
        memory.push((SPCR, table(b"SPCR", 2, &spcr)));
        memory.push((FPDT, fpdt(FBPT, S3PT).to_vec()));
        memory.push((FBPT, fbpt().to_vec()));
        memory.push((S3PT, s3pt().to_vec()));
        let mut entries = Vec::new();
        for addr in [FADT, SPCR, FPDT] {
            entries.extend_from_slice(&addr.to_le_bytes());
        }
        memory.push((XSDT, table(b"XSDT", 1, &entries)));
        let mut rsdp = [0u8; RSDP_LEN];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        memory.push((RSDP, rsdp_naming(&rsdp, XSDT).unwrap().to_vec()));

        let read = |addr: u64, len: usize| -> Option<&[u8]> {
            memory.iter().find_map(|(base, bytes)| {
                let at = usize::try_from(addr.checked_sub(*base)?).ok()?;
                bytes.get(at..at.checked_add(len)?)
            })
        };
        let mut copy = |addr: u64, out: &mut [u8]| -> Result<(), &'static str> {
            out.copy_from_slice(read(addr, out.len()).ok_or("not readable")?);
            Ok(())
        };
        let found = acpi::changing_ranges(RSDP, &mut copy);
        let kinds: Vec<(Changing, u64)> = found.ranges().iter().map(|&(k, b, _)| (k, b)).collect();
        assert_eq!(
            kinds,
            vec![
                (Changing::Facs, FACS),
                (Changing::Fbpt, FBPT),
                (Changing::S3pt, S3PT)
            ]
        );
        let mut located = false;
        let mut lend = |addr: u64, len: usize| read(addr, len).ok_or("not readable");
        let table = acpi::lookup(
            RSDP,
            b"SPCR",
            &mut || core::mem::replace(&mut located, true),
            &mut |_| {},
            &mut copy,
            &mut lend,
        )
        .unwrap();
        assert_eq!(&table[..4], b"SPCR");
        assert_eq!(table.len(), 80);
    }
}
