//! The ACPI I/O Virtualization Reporting Structure, IVRS (CLAUDE.md §32 AMD IOMMU, §58 DMA
//! protection): where the platform's AMD IOMMUs are, which DeviceIDs each one translates —
//! with the aliases, special devices and ACPI-named devices the firmware declares — and the
//! memory ranges some devices must keep reaching.
//!
//! Reference: AMD I/O Virtualization Technology (IOMMU) Specification, publication 48882,
//! Rev 3.11 (April 2026), chapter 5: the IVRS header and its IVinfo field, the IVHD blocks of
//! types 10h, 11h and 40h with their device entries, and the IVMD blocks of types 20h–22h.
//! ACPI 6.6 reserves the signature and defers to that specification.
//!
//! The table is untrusted input. [`parse`] checks the whole table once — the header, then
//! every definition block and every device entry: lengths, placement, alignment, the pairing
//! of range entries and the rules that keep device ownership unambiguous — and refuses it at
//! the first fault, naming the offset. The views it returns are then walked without further
//! checks and cannot panic. A platform may describe one IOMMU several times, once per IVHD
//! type; [`Ivrs::units`] yields each IOMMU once, from the richest block (40h, then 11h, then
//! 10h), as §4.5 of the specification recommends. Pure, no `unsafe`, no allocation.
#![forbid(unsafe_code)]

/// Offset of the first definition block: the 36-byte table header, IVinfo (4 bytes) and 8
/// reserved bytes.
const BLOCKS: usize = 48;
/// Bytes of a definition block's common header (type, flags, length).
const BLOCK_HEADER: usize = 4;
/// Where a type 10h IVHD's device entries start.
const IVHD_LEGACY_HEADER: usize = 24;
/// Where a type 11h or 40h IVHD's device entries start (after the two EFR images).
const IVHD_HEADER: usize = 40;
/// The length of every IVMD.
const IVMD_LENGTH: usize = 32;
/// The fixed part of an ACPI HID device entry, before its UID.
const HID_ENTRY: usize = 22;
/// An IOMMU's register set is at least 16 KiB and aligned to 16 KiB (Capability Offset 04h).
const REGISTER_ALIGN: u64 = 16 * 1024;

/// Definition block types (spec §5.2.2).
pub mod kind {
    /// IVHD with fixed-length device entries, for legacy software.
    pub const IVHD_LEGACY: u8 = 0x10;
    /// IVHD with fixed-length device entries and the EFR images.
    pub const IVHD: u8 = 0x11;
    /// IVHD that may also hold ACPI HID device entries (the mixed format).
    pub const IVHD_MIXED: u8 = 0x40;
    /// IVMD for every peripheral of the IOMMU before it.
    pub const IVMD_ALL: u8 = 0x20;
    /// IVMD for one DeviceID.
    pub const IVMD_DEVICE: u8 = 0x21;
    /// IVMD for a DeviceID range.
    pub const IVMD_RANGE: u8 = 0x22;
}

/// IVHD device entry types (spec Tables 105, 107 and 110).
pub mod entry {
    /// The DTE setting applies to every DeviceID the IOMMU controls.
    pub const ALL: u8 = 0x01;
    /// One DeviceID.
    pub const SELECT: u8 = 0x02;
    /// The first DeviceID of a range; the next entry must be [`RANGE_END`].
    pub const RANGE_START: u8 = 0x03;
    /// The last DeviceID of a range, inclusive.
    pub const RANGE_END: u8 = 0x04;
    /// One DeviceID whose requests carry another DeviceID (the alias).
    pub const ALIAS_SELECT: u8 = 0x42;
    /// The first DeviceID of a range whose requests all carry the alias.
    pub const ALIAS_RANGE_START: u8 = 0x43;
    /// One DeviceID, with extended DTE settings.
    pub const EXTENDED_SELECT: u8 = 0x46;
    /// The first DeviceID of a range with extended DTE settings.
    pub const EXTENDED_RANGE_START: u8 = 0x47;
    /// A device found by firmware rather than enumeration: an I/O APIC or an HPET.
    pub const SPECIAL: u8 = 0x48;
    /// A device named by its ACPI hardware ID (type 40h IVHDs only).
    pub const ACPI_HID: u8 = 0xF0;
}

/// The IVHD flags (spec Tables 92, 97 and 101, one layout for types 10h, 11h and 40h): the
/// control-register settings the platform recommends.
pub mod ivhd_flags {
    pub const HT_TUNNEL: u8 = 1 << 0;
    pub const PASS_PW: u8 = 1 << 1;
    pub const RES_PASS_PW: u8 = 1 << 2;
    pub const ISOC: u8 = 1 << 3;
    pub const IOTLB: u8 = 1 << 4;
    /// Device-table reads should be snooped (the recommended value is 1).
    pub const COHERENT: u8 = 1 << 5;
}

/// The IVinfo field (spec Table 88).
pub mod info {
    /// The IOMMUs have an Extended Feature Register; IVHD types 11h and 40h are allowed.
    pub const EFR_SUPPORTED: u32 = 1 << 0;
    /// The firmware used the IOMMU for pre-boot DMA protection.
    pub const DMA_REMAP: u32 = 1 << 1;
}

/// Why an IVRS was refused. Offsets are from the start of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IvrsError {
    /// The table is shorter than its 48-byte header (the length given).
    Short(usize),
    /// The IVRS revision is neither 1 (fixed DeviceIDs) nor 2 (mixed).
    Revision(u8),
    /// The definition block at this offset is truncated, shorter than its type's minimum or
    /// runs past the table; or it is an IVMD that is not 32 bytes long.
    Block(usize),
    /// The IVHD at this offset has a type the header does not allow: 11h or 40h without
    /// IVinfo's EFRSup, or 40h in a revision-1 table.
    BlockType(usize),
    /// The IVHD at this offset has a register base that is zero or not 16 KiB aligned,
    /// repeats an earlier IVHD of its own type, or shares an earlier IVHD's base with a
    /// different segment or DeviceID.
    RegisterBase(usize),
    /// The device entry at this offset is truncated, of an unknown variable-length type, an
    /// ACPI HID entry outside a type 40h IVHD, or an HID entry whose UID format and length
    /// disagree.
    Entry(usize),
    /// The device entry at this offset breaks range pairing: a range start not followed at
    /// once by an end, an end without a start, or an end below its start.
    Range(usize),
    /// The device entry or IVHD at this offset is a second "all" entry in its IVHD, or a
    /// second IOMMU of its segment that claims every device.
    All(usize),
    /// The IVMD at this offset comes before any IVHD, ends beyond 2^64, names a DeviceID
    /// range that ends before it starts, or asks for an identity mapping that allows neither
    /// reading nor writing.
    Memory(usize),
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        *b.get(at)?,
        *b.get(at.checked_add(1)?)?,
    ]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    let bytes = b.get(at..at.checked_add(4)?)?;
    let mut v = [0u8; 4];
    v.copy_from_slice(bytes);
    Some(u32::from_le_bytes(v))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    let bytes = b.get(at..at.checked_add(8)?)?;
    let mut v = [0u8; 8];
    v.copy_from_slice(bytes);
    Some(u64::from_le_bytes(v))
}

/// The 16-bit DeviceID of a PCI function: bus, then device and function (spec Figure 5).
pub fn device_id(bus: u8, device: u8, function: u8) -> u16 {
    u16::from(bus) << 8 | u16::from(device & 0x1F) << 3 | u16::from(function & 7)
}

/// The bus, device and function a DeviceID names.
pub fn bdf(id: u16) -> (u8, u8, u8) {
    ((id >> 8) as u8, ((id >> 3) & 0x1F) as u8, (id & 7) as u8)
}

/// The DTE settings a device entry asks for (spec Table 106): which special interrupts the
/// devices may send through unmapped, and how system-management messages are handled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DteSetting(pub u8);

impl DteSetting {
    pub fn init_pass(self) -> bool {
        self.0 & 1 != 0
    }

    pub fn ext_int_pass(self) -> bool {
        self.0 & 2 != 0
    }

    pub fn nmi_pass(self) -> bool {
        self.0 & 4 != 0
    }

    /// ATS is permitted for the device (typically an in-SoC device).
    pub fn ats_permitted(self) -> bool {
        self.0 & 8 != 0
    }

    /// The DTE's SysMgt field.
    pub fn system_management(self) -> u8 {
        (self.0 >> 4) & 3
    }

    pub fn lint0_pass(self) -> bool {
        self.0 & 0x40 != 0
    }

    pub fn lint1_pass(self) -> bool {
        self.0 & 0x80 != 0
    }
}

/// What a special device entry describes (spec Table 109).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    /// An I/O APIC; the handle is its APIC ID from the MADT.
    IoApic,
    /// An HPET; the handle is its number from the HPET table.
    Hpet,
    /// A reserved variety (kept, never matched).
    Unknown(u8),
}

impl Special {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::IoApic,
            2 => Self::Hpet,
            other => Self::Unknown(other),
        }
    }
}

/// One device entry of an IVHD, with a range's start and end entries joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceEntry<'a> {
    /// Every DeviceID the IOMMU controls.
    All {
        dte: DteSetting,
    },
    Select {
        id: u16,
        dte: DteSetting,
    },
    Range {
        first: u16,
        last: u16,
        dte: DteSetting,
    },
    /// Device `id`'s requests carry DeviceID `alias`.
    AliasSelect {
        id: u16,
        alias: u16,
        dte: DteSetting,
    },
    /// Every device in `first..=last` sends requests carrying DeviceID `alias`.
    AliasRange {
        first: u16,
        last: u16,
        alias: u16,
        dte: DteSetting,
    },
    ExtendedSelect {
        id: u16,
        dte: DteSetting,
        extended: u32,
    },
    ExtendedRange {
        first: u16,
        last: u16,
        dte: DteSetting,
        extended: u32,
    },
    /// An I/O APIC or HPET (`handle` is its ID or number) whose requests carry DeviceID `id`.
    Special {
        variety: Special,
        handle: u8,
        id: u16,
        dte: DteSetting,
    },
    /// An ACPI-named device whose requests carry DeviceID `id`.
    AcpiHid {
        id: u16,
        dte: DteSetting,
        /// The 8-byte hardware ID and compatible ID fields, as stored.
        hid: &'a [u8],
        cid: &'a [u8],
        /// 0: no UID; 1: an integer; 2: a string.
        uid_format: u8,
        uid: &'a [u8],
    },
    /// A reserved fixed-length type (skipped by its length).
    Reserved {
        kind: u8,
    },
}

/// How an IOMMU sees one device: the DeviceID its requests carry (its own, or an alias) and
/// the DTE settings the firmware asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coverage {
    pub requester: u16,
    pub dte: DteSetting,
}

impl DeviceEntry<'_> {
    /// Whether this entry covers `id` and how: 0 for an entry naming it, 1 for a range, 2 for
    /// "all". Lower ranks win.
    fn cover(&self, id: u16) -> Option<(u8, Coverage)> {
        let own = |dte| Coverage { requester: id, dte };
        match *self {
            Self::Select { id: x, dte }
            | Self::ExtendedSelect { id: x, dte, .. }
            | Self::Special { id: x, dte, .. }
            | Self::AcpiHid { id: x, dte, .. }
                if x == id =>
            {
                Some((0, own(dte)))
            }
            Self::AliasSelect { id: x, alias, dte } if x == id => Some((
                0,
                Coverage {
                    requester: alias,
                    dte,
                },
            )),
            Self::Range { first, last, dte }
            | Self::ExtendedRange {
                first, last, dte, ..
            } if (first..=last).contains(&id) => Some((1, own(dte))),
            Self::AliasRange {
                first,
                last,
                alias,
                dte,
            } if (first..=last).contains(&id) => Some((
                1,
                Coverage {
                    requester: alias,
                    dte,
                },
            )),
            Self::All { dte } => Some((2, own(dte))),
            _ => None,
        }
    }

    /// The highest DeviceID this entry names (an alias included).
    fn last_id(&self) -> Option<u16> {
        match *self {
            Self::All { .. } => Some(u16::MAX),
            Self::Select { id, .. }
            | Self::ExtendedSelect { id, .. }
            | Self::Special { id, .. }
            | Self::AcpiHid { id, .. } => Some(id),
            Self::AliasSelect { id, alias, .. } => Some(id.max(alias)),
            Self::Range { last, .. } | Self::ExtendedRange { last, .. } => Some(last),
            Self::AliasRange { last, alias, .. } => Some(last.max(alias)),
            Self::Reserved { .. } => None,
        }
    }
}

/// The byte length of the device entry at the start of `b`, from its type (spec Table 103);
/// `None` for a variable-length type other than ACPI HID, or an HID entry too short to say.
fn entry_size(b: &[u8]) -> Option<usize> {
    match *b.first()? {
        0x00..=0x3F => Some(4),
        0x40..=0x7F => Some(8),
        entry::ACPI_HID => Some(HID_ENTRY + usize::from(*b.get(21)?)),
        _ => None,
    }
}

/// The device entries of one IVHD (already checked by [`parse`]).
#[derive(Clone, Copy, Debug)]
pub struct Entries<'a> {
    bytes: &'a [u8],
}

impl Entries<'_> {
    /// Consumes `n` bytes, or everything left.
    fn skip(&mut self, n: usize) {
        self.bytes = self.bytes.get(n..).unwrap_or(&[]);
    }
}

impl<'a> Iterator for Entries<'a> {
    type Item = DeviceEntry<'a>;

    fn next(&mut self) -> Option<DeviceEntry<'a>> {
        loop {
            // `parse` refused unknown variable-length types; stopping on one also keeps
            // unchecked bytes from being misread.
            let Some(size) = entry_size(self.bytes) else {
                self.bytes = &[];
                return None;
            };
            let e = self.bytes.get(..size)?;
            self.skip(size);
            let kind = *e.first()?;
            let id = u16_at(e, 1)?;
            let dte = DteSetting(*e.get(3)?);
            return Some(match kind {
                entry::ALL => DeviceEntry::All { dte },
                entry::SELECT => DeviceEntry::Select { id, dte },
                entry::RANGE_START | entry::ALIAS_RANGE_START | entry::EXTENDED_RANGE_START => {
                    // `parse` checked that the end entry follows at once.
                    let end = self.bytes.get(..4)?;
                    if end.first() != Some(&entry::RANGE_END) {
                        self.bytes = &[];
                        return None;
                    }
                    let last = u16_at(end, 1)?;
                    self.skip(4);
                    match kind {
                        entry::RANGE_START => DeviceEntry::Range {
                            first: id,
                            last,
                            dte,
                        },
                        entry::ALIAS_RANGE_START => DeviceEntry::AliasRange {
                            first: id,
                            last,
                            alias: u16_at(e, 5)?,
                            dte,
                        },
                        _ => DeviceEntry::ExtendedRange {
                            first: id,
                            last,
                            dte,
                            extended: u32_at(e, 4)?,
                        },
                    }
                }
                // A stray end of range (`parse` refused it).
                entry::RANGE_END => continue,
                entry::ALIAS_SELECT => DeviceEntry::AliasSelect {
                    id,
                    alias: u16_at(e, 5)?,
                    dte,
                },
                entry::EXTENDED_SELECT => DeviceEntry::ExtendedSelect {
                    id,
                    dte,
                    extended: u32_at(e, 4)?,
                },
                entry::SPECIAL => DeviceEntry::Special {
                    variety: Special::from_byte(*e.get(7)?),
                    handle: *e.get(4)?,
                    id: u16_at(e, 5)?,
                    dte,
                },
                entry::ACPI_HID => DeviceEntry::AcpiHid {
                    id,
                    dte,
                    hid: e.get(4..12)?,
                    cid: e.get(12..20)?,
                    uid_format: *e.get(20)?,
                    uid: e.get(HID_ENTRY..)?,
                },
                other => DeviceEntry::Reserved { kind: other },
            });
        }
    }
}

/// One IOMMU, as one IVHD describes it.
#[derive(Clone, Copy, Debug)]
pub struct Unit<'a> {
    /// The block's offset in the table.
    pub offset: usize,
    /// The IVHD type: [`kind::IVHD_LEGACY`], [`kind::IVHD`] or [`kind::IVHD_MIXED`].
    pub kind: u8,
    /// Recommended control settings ([`ivhd_flags`]).
    pub flags: u8,
    /// The IOMMU's own DeviceID (the PCI function holding its capability block).
    pub device_id: u16,
    /// Where the IOMMU capability block sits in that function's configuration space.
    pub capability_offset: u16,
    /// Physical address of its MMIO registers.
    pub base: u64,
    pub segment: u16,
    /// IOMMU info: the event-log MSI number (bits 4:0) and the UnitID (bits 12:8).
    pub info: u16,
    /// Type 10h: the IOMMU feature reporting field; 11h and 40h: the IOMMU attributes.
    pub attributes: u32,
    /// Types 11h and 40h: an exact copy of the Extended Feature Register and of EFR2.
    pub efr: Option<u64>,
    pub efr2: Option<u64>,
    entries: &'a [u8],
}

impl<'a> Unit<'a> {
    pub fn entries(&self) -> Entries<'a> {
        Entries {
            bytes: self.entries,
        }
    }

    /// The MSI number of the event-log interrupt.
    pub fn msi_number(&self) -> u8 {
        (self.info & 0x1F) as u8
    }

    /// Whether the platform recommends snooped device-table reads.
    pub fn coherent(&self) -> bool {
        self.flags & ivhd_flags::COHERENT != 0
    }

    /// HATDis (types 11h and 40h, attributes bit 0; Tables 98 and 102): the IOMMU does no
    /// host address translation, so it can remap interrupts but cannot confine DMA. Only a
    /// virtual IOMMU reports it; hardware must not.
    pub fn host_translation_disabled(&self) -> bool {
        self.kind != kind::IVHD_LEGACY && self.attributes & 1 != 0
    }

    /// Whether the IOMMU has performance counters, which make its register set 512 KiB
    /// instead of 16 KiB (§2.3): PCSup (bit 9) in the EFR image of a type 11h or 40h block,
    /// or, in a type 10h block, PNBanks (22:17) or PNCounters (16:13) not zero (Table 94).
    pub fn performance_counters(&self) -> bool {
        match self.efr {
            Some(efr) => efr & 1 << 9 != 0,
            None => self.attributes & (0x3F << 17 | 0xF << 13) != 0,
        }
    }

    /// The bytes of physical address space its registers occupy (§2.3).
    pub fn register_span(&self) -> u64 {
        if self.performance_counters() {
            512 * 1024
        } else {
            16 * 1024
        }
    }

    /// How this IOMMU sees `id`: an entry naming it first, then a range holding it, then an
    /// "all" entry; `None` if no entry covers it.
    pub fn covers(&self, id: u16) -> Option<Coverage> {
        self.ranked(id).map(|(_, c)| c)
    }

    fn ranked(&self, id: u16) -> Option<(u8, Coverage)> {
        let mut best: Option<(u8, Coverage)> = None;
        for e in self.entries() {
            if let Some((rank, c)) = e.cover(id)
                && best.is_none_or(|(r, _)| rank < r)
            {
                best = Some((rank, c));
            }
        }
        best
    }

    /// The highest DeviceID any entry names (0xFFFF with an "all" entry): the Device Table
    /// need not reach further.
    pub fn last_device_id(&self) -> Option<u16> {
        self.entries().filter_map(|e| e.last_id()).max()
    }

    /// The DeviceID and DTE settings of the special device `variety` with `handle`.
    pub fn special(&self, variety: Special, handle: u8) -> Option<(u16, DteSetting)> {
        self.entries().find_map(|e| match e {
            DeviceEntry::Special {
                variety: v,
                handle: h,
                id,
                dte,
            } if v == variety && h == handle => Some((id, dte)),
            _ => None,
        })
    }

    fn claims_all(&self) -> bool {
        self.entries().any(|e| matches!(e, DeviceEntry::All { .. }))
    }
}

/// How strongly a block describes its IOMMU: the richest block wins.
fn preference(kind: u8) -> u8 {
    match kind {
        kind::IVHD_MIXED => 3,
        kind::IVHD => 2,
        _ => 1,
    }
}

/// A memory range some devices need (an IVMD), with the IOMMU it pertains to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRange {
    pub offset: usize,
    /// [`kind::IVMD_ALL`], [`kind::IVMD_DEVICE`] or [`kind::IVMD_RANGE`].
    pub kind: u8,
    pub flags: u8,
    /// The DeviceID (21h), or the first of a range (22h).
    pub device_id: u16,
    /// The last DeviceID of a range (22h).
    pub last_device_id: u16,
    pub segment: u16,
    pub start: u64,
    pub length: u64,
    /// The register base and segment of the IOMMU described just before it.
    pub unit_base: u64,
    pub unit_segment: u16,
}

impl MemoryRange {
    /// Device addresses must equal physical ones in this range.
    pub fn unity(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn read(&self) -> bool {
        self.flags & 2 != 0
    }

    pub fn write(&self) -> bool {
        self.flags & 4 != 0
    }

    /// The range belongs to the IOMMU's exclusion range (untranslated).
    pub fn exclusion(&self) -> bool {
        self.flags & 8 != 0
    }

    /// Whether the range must never be mapped (IR and IW clear, not an exclusion range, with
    /// or without Unity: R4, and the review's I2).
    pub fn never_mapped(&self) -> bool {
        !self.read() && !self.write() && !self.exclusion()
    }

    /// Whether the range applies to DeviceID `id` of `segment`.
    pub fn covers(&self, segment: u16, id: u16) -> bool {
        match self.kind {
            kind::IVMD_ALL => segment == self.unit_segment,
            kind::IVMD_DEVICE => segment == self.segment && id == self.device_id,
            _ => segment == self.segment && (self.device_id..=self.last_device_id).contains(&id),
        }
    }
}

/// One definition block.
#[derive(Clone, Copy, Debug)]
pub enum Block<'a> {
    Hardware(Unit<'a>),
    Memory(MemoryRange),
    /// A block type this parser does not know (skipped by its length).
    Other {
        kind: u8,
        offset: usize,
    },
}

/// A checked IVRS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ivrs<'a> {
    /// 1: fixed DeviceIDs only; 2: ACPI HID devices too.
    pub revision: u8,
    /// IVinfo ([`info`]).
    pub info: u32,
    /// The table from its first definition block on.
    body: &'a [u8],
}

/// The definition blocks of a checked IVRS, in table order.
#[derive(Clone, Copy, Debug)]
pub struct Blocks<'a> {
    body: &'a [u8],
    at: usize,
    /// The last IVHD seen: its base and segment, for the IVMDs after it.
    unit: Option<(u64, u16)>,
}

impl<'a> Iterator for Blocks<'a> {
    type Item = Block<'a>;

    fn next(&mut self) -> Option<Block<'a>> {
        let kind = *self.body.get(self.at)?;
        let len = usize::from(u16_at(self.body, self.at.checked_add(2)?)?);
        // `parse` refused shorter blocks; this also keeps a zero length from looping.
        if len < BLOCK_HEADER {
            self.at = self.body.len();
            return None;
        }
        let b = self.body.get(self.at..self.at.checked_add(len)?)?;
        let offset = BLOCKS + self.at;
        self.at += len;
        Some(match kind {
            kind::IVHD_LEGACY | kind::IVHD | kind::IVHD_MIXED => {
                let legacy = kind == kind::IVHD_LEGACY;
                let header = if legacy {
                    IVHD_LEGACY_HEADER
                } else {
                    IVHD_HEADER
                };
                let unit = Unit {
                    offset,
                    kind,
                    flags: *b.get(1)?,
                    device_id: u16_at(b, 4)?,
                    capability_offset: u16_at(b, 6)?,
                    base: u64_at(b, 8)?,
                    segment: u16_at(b, 16)?,
                    info: u16_at(b, 18)?,
                    attributes: u32_at(b, 20)?,
                    efr: if legacy { None } else { u64_at(b, 24) },
                    efr2: if legacy { None } else { u64_at(b, 32) },
                    entries: b.get(header..)?,
                };
                self.unit = Some((unit.base, unit.segment));
                Block::Hardware(unit)
            }
            kind::IVMD_ALL | kind::IVMD_DEVICE | kind::IVMD_RANGE => {
                let (unit_base, unit_segment) = self.unit.unwrap_or((0, 0));
                Block::Memory(MemoryRange {
                    offset,
                    kind,
                    flags: *b.get(1)?,
                    device_id: u16_at(b, 4)?,
                    last_device_id: u16_at(b, 6)?,
                    segment: u16_at(b, 8)?,
                    start: u64_at(b, 16)?,
                    length: u64_at(b, 24)?,
                    unit_base,
                    unit_segment,
                })
            }
            other => Block::Other {
                kind: other,
                offset,
            },
        })
    }
}

impl<'a> Ivrs<'a> {
    /// Every definition block, in table order.
    pub fn blocks(&self) -> Blocks<'a> {
        Blocks {
            body: self.body,
            at: 0,
            unit: None,
        }
    }

    /// Every IVHD, in table order (one IOMMU may appear once per type).
    pub fn hardware(&self) -> impl Iterator<Item = Unit<'a>> + 'a {
        self.blocks().filter_map(|b| match b {
            Block::Hardware(u) => Some(u),
            _ => None,
        })
    }

    /// Each IOMMU once, from its richest IVHD (40h, then 11h, then 10h), in table order.
    pub fn units(&self) -> impl Iterator<Item = Unit<'a>> + 'a {
        let all = *self;
        self.hardware().filter(move |u| {
            !all.hardware()
                .any(|v| v.base == u.base && preference(v.kind) > preference(u.kind))
        })
    }

    /// The memory ranges (IVMDs), in table order.
    pub fn memory(&self) -> impl Iterator<Item = MemoryRange> + 'a {
        self.blocks().filter_map(|b| match b {
            Block::Memory(m) => Some(m),
            _ => None,
        })
    }

    /// The memory ranges that apply to a device of `segment` whose own DeviceID is `id` and
    /// whose requests carry `requester` (its alias, or `id` itself), on the IOMMU whose
    /// registers are at `unit_base` (ADR-0007 Amendment 1, R5, and Amendment 2, R6). An IVMD
    /// for every peripheral (20h) that gives an identity map applies to the devices of the
    /// IOMMU described just before it (48882 §5.2.2); one that keeps its range out of every
    /// device's address space (IR and IW clear, no exclusion) applies to every device in the
    /// system, on every IOMMU (§5.2.2.2; decision item 291). One for a DeviceID (21h) or a range
    /// (22h) applies by its own segment and DeviceID, whichever IOMMU translates the device, and
    /// whether it names the device's own DeviceID or the one its requests carry.
    pub fn memory_for(
        &self,
        unit_base: u64,
        segment: u16,
        id: u16,
        requester: u16,
    ) -> impl Iterator<Item = MemoryRange> + 'a {
        self.memory().filter(move |m| match m.kind {
            kind::IVMD_ALL => {
                m.never_mapped() || (m.unit_base == unit_base && m.unit_segment == segment)
            }
            _ => m.covers(segment, id) || m.covers(segment, requester),
        })
    }

    /// The IOMMUs have an Extended Feature Register.
    pub fn efr_supported(&self) -> bool {
        self.info & info::EFR_SUPPORTED != 0
    }

    /// The firmware used the IOMMUs for pre-boot DMA protection.
    pub fn dma_remap(&self) -> bool {
        self.info & info::DMA_REMAP != 0
    }

    /// The width of system physical addresses, in bits (IVinfo PAsize).
    pub fn physical_bits(&self) -> u8 {
        ((self.info >> 8) & 0x7F) as u8
    }

    /// The width of guest physical addresses, in bits (IVinfo VAsize).
    pub fn virtual_bits(&self) -> u8 {
        ((self.info >> 15) & 0x7F) as u8
    }

    /// The IOMMU that translates DeviceID `id` of `segment`, and how it sees the device: an
    /// IOMMU whose entry names the device wins over one whose range holds it, which wins
    /// over one with an "all" entry; the first in table order breaks a tie. `None`: no IOMMU
    /// translates the device, so its DMA is not remapped.
    pub fn unit_for(&self, segment: u16, id: u16) -> Option<(Unit<'a>, Coverage)> {
        let mut best: Option<(u8, Unit<'a>, Coverage)> = None;
        for u in self.units().filter(|u| u.segment == segment) {
            if let Some((rank, c)) = u.ranked(id)
                && best.as_ref().is_none_or(|(r, _, _)| rank < *r)
            {
                best = Some((rank, u, c));
            }
        }
        best.map(|(_, u, c)| (u, c))
    }

    /// The IOMMU and DeviceID of the special device `variety` with `handle`.
    pub fn special(&self, variety: Special, handle: u8) -> Option<(Unit<'a>, u16, DteSetting)> {
        self.units()
            .find_map(|u| u.special(variety, handle).map(|(id, dte)| (u, id, dte)))
    }
}

/// Checks an IVRS (the whole table, `acpi::check_table(b"IVRS")` already passed) and returns
/// a view of it. Every rule below is checked here, once:
/// - the header is complete and the revision is 1 or 2;
/// - every definition block has a header, is at least its type's minimum length (IVHD 10h 24
///   bytes, 11h and 40h 40, IVMD exactly 32) and ends inside the table; unknown types are
///   skipped by their length;
/// - an IVHD of type 11h or 40h needs IVinfo's EFRSup, and type 40h needs revision 2;
/// - an IVHD's register base is non-zero and 16 KiB aligned; a second IVHD with the same
///   base must be of another type and name the same segment and DeviceID (it describes the
///   same IOMMU);
/// - every device entry fits its IVHD and has a known length; ACPI HID entries appear only
///   in type 40h IVHDs, with a UID format of 0–2 that agrees with the UID length;
/// - every range start is followed at once by an end at or above it, and no end stands alone;
/// - at most one "all" entry per IVHD, and at most one IOMMU per segment that claims all;
/// - an IVMD follows some IVHD, ends below 2^64, has a range that does not end before it
///   starts, and asks for no identity mapping without read or write access.
pub fn parse(table: &[u8]) -> Result<Ivrs<'_>, IvrsError> {
    let body = table.get(BLOCKS..).ok_or(IvrsError::Short(table.len()))?;
    let revision = table.get(8).copied().unwrap_or(0);
    if !(1..=2).contains(&revision) {
        return Err(IvrsError::Revision(revision));
    }
    let ivrs = Ivrs {
        revision,
        info: u32_at(table, 36).unwrap_or(0),
        body,
    };
    let mut at = 0usize;
    let mut described = false;
    while at < body.len() {
        let offset = BLOCKS + at;
        let bad = IvrsError::Block(offset);
        let kind = *body.get(at).ok_or(bad)?;
        let len = usize::from(u16_at(body, at + 2).ok_or(bad)?);
        let ivmd = matches!(kind, kind::IVMD_ALL | kind::IVMD_DEVICE | kind::IVMD_RANGE);
        let minimum = match kind {
            kind::IVHD_LEGACY => IVHD_LEGACY_HEADER,
            kind::IVHD | kind::IVHD_MIXED => IVHD_HEADER,
            _ if ivmd => IVMD_LENGTH,
            _ => BLOCK_HEADER,
        };
        if len < minimum || (ivmd && len != IVMD_LENGTH) {
            return Err(bad);
        }
        let b = body.get(at..at + len).ok_or(bad)?;
        match kind {
            kind::IVHD_LEGACY | kind::IVHD | kind::IVHD_MIXED => {
                check_unit(&ivrs, b, kind, offset, at)?;
                described = true;
            }
            _ if ivmd => {
                if !described {
                    return Err(IvrsError::Memory(offset));
                }
                check_memory(b, kind, offset)?;
            }
            _ => {}
        }
        at += len;
    }
    Ok(ivrs)
}

/// Checks the IVHD `b` of type `kind` at table offset `offset` (body offset `at`): its type
/// against the header, its register base against the IVHDs before it, its entries, and the
/// one-"all"-IOMMU-per-segment rule.
fn check_unit(
    ivrs: &Ivrs<'_>,
    b: &[u8],
    kind: u8,
    offset: usize,
    at: usize,
) -> Result<(), IvrsError> {
    if kind != kind::IVHD_LEGACY && !ivrs.efr_supported() {
        return Err(IvrsError::BlockType(offset));
    }
    if kind == kind::IVHD_MIXED && ivrs.revision < 2 {
        return Err(IvrsError::BlockType(offset));
    }
    let bad = IvrsError::Block(offset);
    let base = u64_at(b, 8).ok_or(bad)?;
    let device_id = u16_at(b, 4).ok_or(bad)?;
    let segment = u16_at(b, 16).ok_or(bad)?;
    if base == 0 || !base.is_multiple_of(REGISTER_ALIGN) {
        return Err(IvrsError::RegisterBase(offset));
    }
    let earlier = Ivrs {
        body: ivrs.body.get(..at).unwrap_or(&[]),
        ..*ivrs
    };
    for u in earlier.hardware() {
        if u.base == base && (u.kind == kind || u.segment != segment || u.device_id != device_id) {
            return Err(IvrsError::RegisterBase(offset));
        }
    }
    let from = if kind == kind::IVHD_LEGACY {
        IVHD_LEGACY_HEADER
    } else {
        IVHD_HEADER
    };
    let all = check_entries(b, from, offset, kind)?;
    if all
        && earlier
            .hardware()
            .any(|u| u.base != base && u.segment == segment && u.claims_all())
    {
        return Err(IvrsError::All(offset));
    }
    Ok(())
}

/// Checks the device entries of the IVHD `b` (of type `kind`, at table offset `offset`),
/// which start at `from` within it. Returns whether the IVHD has an "all" entry.
fn check_entries(b: &[u8], from: usize, offset: usize, kind: u8) -> Result<bool, IvrsError> {
    let mut at = from;
    // An open range: its start entry's offset and first DeviceID.
    let mut open: Option<(usize, u16)> = None;
    let mut all = false;
    while at < b.len() {
        let here = offset + at;
        let rest = b.get(at..).unwrap_or(&[]);
        let size = entry_size(rest).ok_or(IvrsError::Entry(here))?;
        let e = rest.get(..size).ok_or(IvrsError::Entry(here))?;
        let ty = *e.first().ok_or(IvrsError::Entry(here))?;
        let id = u16_at(e, 1).ok_or(IvrsError::Entry(here))?;
        if ty == entry::ACPI_HID {
            let format = *e.get(20).ok_or(IvrsError::Entry(here))?;
            let uid = *e.get(21).ok_or(IvrsError::Entry(here))?;
            if kind != kind::IVHD_MIXED || format > 2 || (format == 0) != (uid == 0) {
                return Err(IvrsError::Entry(here));
            }
        }
        match open.take() {
            Some((start, first)) => {
                if ty != entry::RANGE_END {
                    return Err(IvrsError::Range(start));
                }
                if id < first {
                    return Err(IvrsError::Range(here));
                }
            }
            None => match ty {
                entry::RANGE_END => return Err(IvrsError::Range(here)),
                entry::RANGE_START | entry::ALIAS_RANGE_START | entry::EXTENDED_RANGE_START => {
                    open = Some((here, id));
                }
                entry::ALL => {
                    if all {
                        return Err(IvrsError::All(here));
                    }
                    all = true;
                }
                _ => {}
            },
        }
        at += size;
    }
    match open {
        Some((start, _)) => Err(IvrsError::Range(start)),
        None => Ok(all),
    }
}

/// Checks the IVMD `b` of type `kind` at table offset `offset`.
fn check_memory(b: &[u8], kind: u8, offset: usize) -> Result<(), IvrsError> {
    let bad = IvrsError::Memory(offset);
    let start = u64_at(b, 16).ok_or(bad)?;
    let length = u64_at(b, 24).ok_or(bad)?;
    if start.checked_add(length).is_none() {
        return Err(bad);
    }
    if kind == kind::IVMD_RANGE && u16_at(b, 6).ok_or(bad)? < u16_at(b, 4).ok_or(bad)? {
        return Err(bad);
    }
    // Unity without read or write access (outside an exclusion range) is accepted: Table 113
    // asks firmware for Unity clear there, and R4 reads "IR and IW both clear" as a range never
    // mapped, whatever Unity says (`reserved::from_ivmd`; TASKS item 40's review, I2), rather
    // than as a reason to withhold every driver.
    Ok(())
}

/// Test builders, shared with the kernel-model tests elsewhere in the crate.
#[cfg(test)]
pub(crate) mod build {
    use alloc::vec::Vec;

    /// A 4-byte device entry.
    pub(crate) fn e4(kind: u8, id: u16, dte: u8) -> Vec<u8> {
        let id = id.to_le_bytes();
        alloc::vec![kind, id[0], id[1], dte]
    }

    /// An 8-byte device entry: type, DeviceID, DTE setting, byte 4, the second DeviceID (bytes
    /// 5–6) and byte 7.
    pub(crate) fn e8(kind: u8, id: u16, dte: u8, b4: u8, id2: u16, b7: u8) -> Vec<u8> {
        let (a, b) = (id.to_le_bytes(), id2.to_le_bytes());
        alloc::vec![kind, a[0], a[1], dte, b4, b[0], b[1], b7]
    }

    /// An extended entry (46h or 47h) with its 32-bit extended DTE setting.
    pub(crate) fn extended(kind: u8, id: u16, dte: u8, ext: u32) -> Vec<u8> {
        let mut e = e4(kind, id, dte);
        e.extend_from_slice(&ext.to_le_bytes());
        e
    }

    /// An ACPI HID entry with a UID of `format`.
    pub(crate) fn hid(
        id: u16,
        dte: u8,
        hid: &[u8; 8],
        cid: &[u8; 8],
        format: u8,
        uid: &[u8],
    ) -> Vec<u8> {
        let mut e = e4(super::entry::ACPI_HID, id, dte);
        e.extend_from_slice(hid);
        e.extend_from_slice(cid);
        e.push(format);
        e.push(uid.len() as u8);
        e.extend_from_slice(uid);
        e
    }

    /// An IVHD of `kind` (10h: a 24-byte header; 11h and 40h: 40 with the EFR images).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ivhd(
        kind: u8,
        flags: u8,
        device_id: u16,
        capability: u16,
        base: u64,
        segment: u16,
        info: u16,
        attributes: u32,
        efr: u64,
        efr2: u64,
        entries: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut b = alloc::vec![kind, flags, 0, 0];
        b.extend_from_slice(&device_id.to_le_bytes());
        b.extend_from_slice(&capability.to_le_bytes());
        b.extend_from_slice(&base.to_le_bytes());
        b.extend_from_slice(&segment.to_le_bytes());
        b.extend_from_slice(&info.to_le_bytes());
        b.extend_from_slice(&attributes.to_le_bytes());
        if kind != super::kind::IVHD_LEGACY {
            b.extend_from_slice(&efr.to_le_bytes());
            b.extend_from_slice(&efr2.to_le_bytes());
        }
        for e in entries {
            b.extend_from_slice(e);
        }
        let len = b.len() as u16;
        b[2..4].copy_from_slice(&len.to_le_bytes());
        b
    }

    /// An IVMD.
    pub(crate) fn ivmd(
        kind: u8,
        flags: u8,
        id: u16,
        last: u16,
        segment: u16,
        start: u64,
        length: u64,
    ) -> Vec<u8> {
        let mut b = alloc::vec![kind, flags, 32, 0];
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&last.to_le_bytes());
        b.extend_from_slice(&segment.to_le_bytes());
        b.extend_from_slice(&[0; 6]);
        b.extend_from_slice(&start.to_le_bytes());
        b.extend_from_slice(&length.to_le_bytes());
        b
    }

    /// A whole IVRS (header, `revision`, IVinfo, then `blocks`), with a correct length and
    /// checksum.
    pub(crate) fn table(revision: u8, info: u32, blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut t = alloc::vec![0u8; 48];
        t[..4].copy_from_slice(b"IVRS");
        t[8] = revision;
        t[36..40].copy_from_slice(&info.to_le_bytes());
        for b in blocks {
            t.extend_from_slice(b);
        }
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        crate::dmar::build::seal(&mut t);
        t
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;
    use alloc::vec::Vec;

    /// The IVRS QEMU 11.1.3 builds for this repository's AMD boot (`ORIGIN_QEMU_IOMMU=amd`: q35,
    /// the IOMMU's function pinned at 00:05.0, `amd-iommu,intremap=on,xtsup=on,dma-remap=on`),
    /// byte for byte as the kernel read it from the firmware: revision 1, IVinfo 0x2801, and
    /// one IOMMU at 0xfed80000 described twice — an IVHD 10h and an IVHD 11h — each selecting
    /// the eight PCI functions and naming the I/O APIC as DeviceID 0x00a0.
    const QEMU_IVRS: [u8; 192] = [
        0x49, 0x56, 0x52, 0x53, 0xC0, 0x00, 0x00, 0x00, 0x01, 0x97, 0x42, 0x4F, 0x43, 0x48, 0x53,
        0x20, 0x42, 0x58, 0x50, 0x43, 0x20, 0x20, 0x20, 0x20, 0x01, 0x00, 0x00, 0x00, 0x42, 0x58,
        0x50, 0x43, 0x01, 0x00, 0x00, 0x00, 0x01, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x10, 0xD1, 0x40, 0x00, 0x28, 0x00, 0x40, 0x00, 0x00, 0x00, 0xD8, 0xFE,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x45, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
        0x00, 0x02, 0x08, 0x00, 0x00, 0x02, 0x10, 0x00, 0x00, 0x02, 0x18, 0x00, 0x00, 0x02, 0x28,
        0x00, 0x00, 0x02, 0xF8, 0x00, 0x00, 0x02, 0xFA, 0x00, 0x00, 0x02, 0xFB, 0x00, 0x00, 0x48,
        0x00, 0x00, 0x00, 0x00, 0xA0, 0x00, 0x01, 0x11, 0x11, 0x50, 0x00, 0x28, 0x00, 0x40, 0x00,
        0x00, 0x00, 0xD8, 0xFE, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xD7, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x00, 0x00, 0x02, 0x10, 0x00, 0x00, 0x02,
        0x18, 0x00, 0x00, 0x02, 0x28, 0x00, 0x00, 0x02, 0xF8, 0x00, 0x00, 0x02, 0xFA, 0x00, 0x00,
        0x02, 0xFB, 0x00, 0x00, 0x48, 0x00, 0x00, 0x00, 0x00, 0xA0, 0x00, 0x01,
    ];

    #[test]
    fn the_table_qemu_builds() {
        assert_eq!(crate::acpi::check_table(&QEMU_IVRS, b"IVRS"), Ok(()));
        let r = parse(&QEMU_IVRS).unwrap();
        assert_eq!((r.revision, r.info), (1, 0x2801));
        assert!(r.efr_supported() && !r.dma_remap());
        assert_eq!(r.physical_bits(), 40);
        let kinds: Vec<u8> = r.hardware().map(|u| u.kind).collect();
        assert_eq!(kinds, [kind::IVHD_LEGACY, kind::IVHD]);
        let units: Vec<_> = r.units().collect();
        assert_eq!(units.len(), 1, "one IOMMU, from its 11h block");
        let u = units[0];
        assert_eq!(
            (
                u.kind,
                u.flags,
                u.device_id,
                u.capability_offset,
                u.base,
                u.segment
            ),
            (kind::IVHD, 0x11, device_id(0, 5, 0), 0x40, 0xFED8_0000, 0)
        );
        assert_eq!((u.efr, u.efr2, u.attributes), (Some(0x29D7), Some(0), 0));
        assert!(!u.coherent());
        let selected: Vec<u16> = u
            .entries()
            .filter_map(|e| match e {
                DeviceEntry::Select { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(selected, [0x00, 0x08, 0x10, 0x18, 0x28, 0xF8, 0xFA, 0xFB]);
        assert_eq!(u.last_device_id(), Some(0xFB));
        assert_eq!(u.special(Special::IoApic, 0), Some((0x00A0, DteSetting(0))));
        // The NVMe controller (00:02.0) is translated as itself; 00:04.0 is not listed.
        let nvme = r
            .unit_for(0, device_id(0, 2, 0))
            .map(|(u, c)| (u.base, c.requester));
        assert_eq!(nvme, Some((0xFED8_0000, 0x0010)));
        assert!(r.unit_for(0, device_id(0, 4, 0)).is_none());
        // The legacy block describes the same IOMMU and devices.
        let legacy = r.hardware().next().unwrap();
        assert_eq!(
            (legacy.flags, legacy.attributes, legacy.efr),
            (0xD1, 0x45, None)
        );
        assert_eq!(legacy.entries().count(), 9);
        assert_eq!(r.memory().count(), 0);
    }

    /// A two-IOMMU platform in two segments, described with every block and entry type: IOMMU
    /// A (segment 0) as a 10h block and again as a 40h block, IOMMU B (segment 1) as an 11h
    /// block with an "all" entry, and IVMDs after each.
    fn platform() -> Vec<u8> {
        let a_entries = |mixed: bool| {
            let mut v = alloc::vec![
                e4(entry::SELECT, device_id(0, 0, 0), 0),
                e4(entry::RANGE_START, device_id(0, 1, 0), 0),
                e4(entry::RANGE_END, device_id(0, 0x1F, 7), 0),
                // 01:00.0 behind a PCIe-to-PCI bridge: 02:00.0–02:1f.7 carry 01:00.0.
                e8(
                    entry::ALIAS_RANGE_START,
                    device_id(2, 0, 0),
                    0,
                    0,
                    device_id(1, 0, 0),
                    0
                ),
                e4(entry::RANGE_END, device_id(2, 0x1F, 7), 0),
                e8(
                    entry::ALIAS_SELECT,
                    device_id(3, 0, 0),
                    0,
                    0,
                    device_id(3, 0, 1),
                    0
                ),
                extended(entry::EXTENDED_SELECT, device_id(4, 0, 0), 0x08, 1 << 31),
                extended(entry::EXTENDED_RANGE_START, device_id(5, 0, 0), 0, 0),
                e4(entry::RANGE_END, device_id(5, 0, 7), 0),
                e8(entry::SPECIAL, 0, 0xD7, 0x21, device_id(0, 0x14, 0), 1),
                e8(entry::SPECIAL, 0, 0, 0, device_id(0, 0x14, 1), 2),
                e4(0x05, 0x1234, 0),     // reserved 4-byte type
                e8(0x49, 0, 0, 0, 0, 0), // reserved 8-byte type
            ];
            if mixed {
                v.push(hid(
                    device_id(0, 0x14, 5),
                    0,
                    b"AMDI0040",
                    &[0; 8],
                    2,
                    b"\\_SB.FUR0",
                ));
                v.push(hid(
                    device_id(0, 0x14, 6),
                    0,
                    b"AMDI0020",
                    b"PNP0C02\0",
                    0,
                    b"",
                ));
            }
            v
        };
        let a10 = ivhd(
            kind::IVHD_LEGACY,
            0x30,
            device_id(0, 0, 2),
            0x40,
            0xFEB8_0000,
            0,
            0x0100,
            0,
            0,
            0,
            &a_entries(false),
        );
        let a40 = ivhd(
            kind::IVHD_MIXED,
            0x20,
            device_id(0, 0, 2),
            0x40,
            0xFEB8_0000,
            0,
            0x0100,
            0,
            0x0000_0045_A5BF_E000,
            0x0100,
            &a_entries(true),
        );
        let b11 = ivhd(
            kind::IVHD,
            0x20,
            device_id(0x40, 0, 2),
            0x40,
            0xFEC0_4000,
            1,
            0,
            0,
            0x245,
            0,
            &[e4(entry::ALL, 0, 0)],
        );
        let reserved_bios = ivmd(kind::IVMD_ALL, 0b0111, 0, 0, 0, 0x7A00_0000, 0x10_0000);
        let usb = ivmd(
            kind::IVMD_DEVICE,
            0b0111,
            device_id(0, 0x13, 0),
            0,
            0,
            0x7B00_0000,
            0x1000,
        );
        let gpus = ivmd(
            kind::IVMD_RANGE,
            0b1000,
            device_id(1, 0, 0),
            device_id(1, 0, 1),
            1,
            0x7C00_0000,
            0x2000,
        );
        let unknown = alloc::vec![0x99, 0, 6, 0, 0xAA, 0xBB];
        table(
            2,
            info::EFR_SUPPORTED | 48 << 8 | 64 << 15,
            &[a10, reserved_bios, a40, usb, unknown, b11, gpus],
        )
    }

    /// Two IOMMUs in one segment, with IVMDs placed after the second (R5): a 21h naming a
    /// device of the first still applies to it; the second's "all" range does not.
    #[test]
    fn a_device_range_applies_wherever_it_sits() {
        let a = ivhd(
            kind::IVHD,
            0,
            device_id(0, 0, 2),
            0x40,
            0xFEB8_0000,
            0,
            0,
            0,
            0,
            0,
            &[e4(entry::SELECT, device_id(0, 3, 0), 0)],
        );
        let b = ivhd(
            kind::IVHD,
            0,
            device_id(0x80, 0, 2),
            0x40,
            0xFEC0_0000,
            0,
            0,
            0,
            0,
            0,
            &[e4(entry::SELECT, device_id(0x80, 1, 0), 0)],
        );
        let nvme = device_id(0, 3, 0);
        let mine = ivmd(kind::IVMD_DEVICE, 0b0111, nvme, 0, 0, 0x7D00_0000, 0x1000);
        let all_b = ivmd(kind::IVMD_ALL, 0b0011, 0, 0, 0, 0x7E00_0000, 0x1000);
        let t = table(1, info::EFR_SUPPORTED, &[a, b, mine, all_b]);
        let r = parse(&t).unwrap();
        let (unit, c) = r.unit_for(0, nvme).unwrap();
        assert_eq!(unit.base, 0xFEB8_0000);
        let starts: Vec<u64> = r
            .memory_for(unit.base, 0, nvme, c.requester)
            .map(|m| m.start)
            .collect();
        assert_eq!(starts, alloc::vec![0x7D00_0000]);
        let other = device_id(0x80, 1, 0);
        let starts: Vec<u64> = r
            .memory_for(0xFEC0_0000, 0, other, other)
            .map(|m| m.start)
            .collect();
        assert_eq!(starts, alloc::vec![0x7E00_0000]);
    }

    #[test]
    fn a_never_map_range_for_every_peripheral_applies_on_every_iommu() {
        let unit = |base, bus, dev| {
            ivhd(
                kind::IVHD,
                0,
                device_id(bus, 0, 2),
                0x40,
                base,
                0,
                0,
                0,
                0,
                0,
                &[e4(entry::SELECT, device_id(bus, dev, 0), 0)],
            )
        };
        let (a, b) = (unit(0xFEB8_0000, 0, 3), unit(0xFEC0_0000, 0x80, 1));
        // After A: an identity 20h (R5: A's devices only) and a never-map 20h (R6: everyone's),
        // with Unity set as well as without it.
        let identity = ivmd(kind::IVMD_ALL, 0b0011, 0, 0, 0, 0x7E00_0000, 0x1000);
        let never = ivmd(kind::IVMD_ALL, 0b0000, 0, 0, 0, 0x7F00_0000, 0x1000);
        let never_unity = ivmd(kind::IVMD_ALL, 0b0001, 0, 0, 0, 0x7F10_0000, 0x1000);
        // An exclusion range with IR = IW = 0 is an identity map (R4), so it keeps R5.
        let exclusion = ivmd(kind::IVMD_ALL, 0b1000, 0, 0, 0, 0x7F20_0000, 0x1000);
        let t = table(
            1,
            info::EFR_SUPPORTED,
            &[a, identity, never, never_unity, exclusion, b],
        );
        let r = parse(&t).unwrap();
        let on_a = device_id(0, 3, 0);
        let starts: Vec<u64> = r
            .memory_for(0xFEB8_0000, 0, on_a, on_a)
            .map(|m| m.start)
            .collect();
        assert_eq!(
            starts,
            alloc::vec![0x7E00_0000, 0x7F00_0000, 0x7F10_0000, 0x7F20_0000]
        );
        let on_b = device_id(0x80, 1, 0);
        let starts: Vec<u64> = r
            .memory_for(0xFEC0_0000, 0, on_b, on_b)
            .map(|m| m.start)
            .collect();
        assert_eq!(starts, alloc::vec![0x7F00_0000, 0x7F10_0000]);
        // And on an IOMMU of another segment too: "every device, on every IOMMU".
        let starts: Vec<u64> = r
            .memory_for(0xFED0_0000, 1, on_b, on_b)
            .map(|m| m.start)
            .collect();
        assert_eq!(starts, alloc::vec![0x7F00_0000, 0x7F10_0000]);
    }

    #[test]
    fn every_block_and_entry_type() {
        let t = platform();
        assert_eq!(crate::acpi::check_table(&t, b"IVRS"), Ok(()));
        let r = parse(&t).unwrap();
        assert_eq!(r.revision, 2);
        assert!(r.efr_supported() && !r.dma_remap());
        assert_eq!((r.physical_bits(), r.virtual_bits()), (48, 64));
        let kinds: Vec<u8> = r
            .blocks()
            .map(|b| match b {
                Block::Hardware(u) => u.kind,
                Block::Memory(m) => m.kind,
                Block::Other { kind, .. } => kind,
            })
            .collect();
        assert_eq!(kinds, [0x10, 0x20, 0x40, 0x21, 0x99, 0x11, 0x22]);
        assert_eq!(r.hardware().count(), 3);
        // IOMMU A once, from its 40h block; IOMMU B from its 11h block.
        let units: Vec<_> = r.units().collect();
        assert_eq!(units.len(), 2);
        let (a, b) = (units[0], units[1]);
        assert_eq!(
            (a.kind, a.base, a.segment),
            (kind::IVHD_MIXED, 0xFEB8_0000, 0)
        );
        assert_eq!(
            (a.device_id, a.capability_offset),
            (device_id(0, 0, 2), 0x40)
        );
        assert_eq!((a.efr, a.efr2), (Some(0x0000_0045_A5BF_E000), Some(0x100)));
        assert!(a.coherent());
        assert_eq!(
            (b.kind, b.base, b.segment, b.efr),
            (kind::IVHD, 0xFEC0_4000, 1, Some(0x245))
        );
        // Every entry type decodes, ranges joined.
        let e: Vec<_> = a.entries().collect();
        assert_eq!(e.len(), 12);
        assert_eq!(
            e[1],
            DeviceEntry::Range {
                first: 0x0008,
                last: 0x00FF,
                dte: DteSetting(0)
            }
        );
        assert_eq!(
            e[2],
            DeviceEntry::AliasRange {
                first: 0x0200,
                last: 0x02FF,
                alias: 0x0100,
                dte: DteSetting(0)
            }
        );
        assert_eq!(
            e[4],
            DeviceEntry::ExtendedSelect {
                id: 0x0400,
                dte: DteSetting(0x08),
                extended: 1 << 31
            }
        );
        assert_eq!(
            e[6],
            DeviceEntry::Special {
                variety: Special::IoApic,
                handle: 0x21,
                id: 0x00A0,
                dte: DteSetting(0xD7)
            }
        );
        assert_eq!(e[7].last_id(), Some(0x00A1));
        assert_eq!(e[8], DeviceEntry::Reserved { kind: 0x05 });
        assert_eq!(e[9], DeviceEntry::Reserved { kind: 0x49 });
        match e[10] {
            DeviceEntry::AcpiHid {
                id,
                hid,
                uid_format,
                uid,
                ..
            } => {
                assert_eq!(
                    (id, hid, uid_format, uid),
                    (0x00A5, &b"AMDI0040"[..], 2, &b"\\_SB.FUR0"[..])
                );
            }
            other => panic!("{other:?}"),
        }
        let d = DteSetting(0xD7);
        assert!(d.init_pass() && d.ext_int_pass() && d.nmi_pass() && !d.ats_permitted());
        assert!(d.lint0_pass() && d.lint1_pass());
        assert_eq!(d.system_management(), 1);
        assert_eq!(a.last_device_id(), Some(0x0507));
        assert_eq!(b.last_device_id(), Some(0xFFFF));
    }

    #[test]
    fn devices_resolve_to_their_iommu_and_requester() {
        let t = platform();
        let r = parse(&t).unwrap();
        let unit = |seg, id| r.unit_for(seg, id).map(|(u, c)| (u.base, c.requester));
        // Named, in a range, behind an alias range, an alias select, an extended range.
        assert_eq!(unit(0, 0x0000), Some((0xFEB8_0000, 0x0000)));
        assert_eq!(unit(0, device_id(0, 2, 0)), Some((0xFEB8_0000, 0x0010)));
        assert_eq!(unit(0, device_id(2, 3, 1)), Some((0xFEB8_0000, 0x0100)));
        assert_eq!(unit(0, device_id(3, 0, 0)), Some((0xFEB8_0000, 0x0301)));
        assert_eq!(unit(0, device_id(5, 0, 7)), Some((0xFEB8_0000, 0x0507)));
        // Not covered in segment 0; everything in segment 1 falls to B's "all" entry.
        assert_eq!(unit(0, device_id(6, 0, 0)), None);
        assert_eq!(unit(1, device_id(6, 0, 0)), Some((0xFEC0_4000, 0x0600)));
        assert_eq!(unit(2, 0), None);
        // Special devices.
        let (u, id, dte) = r.special(Special::IoApic, 0x21).unwrap();
        assert_eq!((u.base, id, dte), (0xFEB8_0000, 0x00A0, DteSetting(0xD7)));
        assert_eq!(
            r.special(Special::Hpet, 0).map(|(_, id, _)| id),
            Some(0x00A1)
        );
        assert!(r.special(Special::IoApic, 0).is_none());
        // The memory ranges and what they cover.
        let m: Vec<_> = r.memory().collect();
        assert_eq!(m.len(), 3);
        assert!(m[0].unity() && m[0].read() && m[0].write() && !m[0].exclusion());
        assert_eq!((m[0].unit_base, m[0].unit_segment), (0xFEB8_0000, 0));
        assert!(m[0].covers(0, 0x1234) && !m[0].covers(1, 0x1234));
        assert!(m[1].covers(0, device_id(0, 0x13, 0)) && !m[1].covers(0, device_id(0, 0x13, 1)));
        assert!(m[2].exclusion());
        assert_eq!(m[2].unit_base, 0xFEC0_4000);
        assert!(m[2].covers(1, device_id(1, 0, 1)) && !m[2].covers(1, device_id(1, 0, 2)));
        assert!(!m[2].covers(0, device_id(1, 0, 0)));
        // Which ranges apply to a device (R5): the "all" range of the IOMMU before it, and
        // the ranges naming it or its alias, wherever they sit.
        let starts = |unit, segment, id, requester| {
            r.memory_for(unit, segment, id, requester)
                .map(|m| m.start)
                .collect::<Vec<_>>()
        };
        let usb = device_id(0, 0x13, 0);
        assert_eq!(
            starts(0xFEB8_0000, 0, usb, usb),
            alloc::vec![0x7A00_0000, 0x7B00_0000]
        );
        let other = device_id(0, 0x13, 1);
        assert_eq!(
            starts(0xFEB8_0000, 0, other, other),
            alloc::vec![0x7A00_0000]
        );
        // On IOMMU B the "all" range of A does not apply; the 22h range does, and an alias
        // that the range names brings a device into it.
        let gpu = device_id(1, 0, 1);
        assert_eq!(starts(0xFEC0_4000, 1, gpu, gpu), alloc::vec![0x7C00_0000]);
        let behind = device_id(1, 0, 4);
        assert_eq!(starts(0xFEC0_4000, 1, behind, behind), alloc::vec![]);
        assert_eq!(
            starts(0xFEC0_4000, 1, behind, gpu),
            alloc::vec![0x7C00_0000]
        );
        assert_eq!(bdf(device_id(0x12, 0x1F, 7)), (0x12, 0x1F, 7));
    }

    #[test]
    fn a_named_entry_beats_a_range_and_a_range_beats_all() {
        let entries = [
            e4(entry::ALL, 0, 0x01),
            e4(entry::RANGE_START, 0x0010, 0x02),
            e4(entry::RANGE_END, 0x0017, 0),
            e8(entry::ALIAS_SELECT, 0x0012, 0x04, 0, 0x0099, 0),
        ];
        let t = table(
            1,
            0,
            &[ivhd(
                kind::IVHD_LEGACY,
                0,
                2,
                0x40,
                0x4000,
                0,
                0,
                0,
                0,
                0,
                &entries,
            )],
        );
        let r = parse(&t).unwrap();
        let u = r.units().next().unwrap();
        assert_eq!(
            u.covers(0x0012),
            Some(Coverage {
                requester: 0x0099,
                dte: DteSetting(4)
            })
        );
        assert_eq!(
            u.covers(0x0013),
            Some(Coverage {
                requester: 0x0013,
                dte: DteSetting(2)
            })
        );
        assert_eq!(
            u.covers(0x0100),
            Some(Coverage {
                requester: 0x0100,
                dte: DteSetting(1)
            })
        );
        assert!(u.efr.is_none());
    }

    #[test]
    fn host_translation_and_register_span_follow_the_block_type() {
        let sel = e4(entry::SELECT, 0x10, 0);
        let one = |kind: u8, attributes: u32, efr: u64| {
            let t = table(
                2,
                info::EFR_SUPPORTED,
                &[ivhd(
                    kind,
                    0,
                    2,
                    0x40,
                    0xFEB8_0000,
                    0,
                    0,
                    attributes,
                    efr,
                    0,
                    core::slice::from_ref(&sel),
                )],
            );
            let u = parse(&t).unwrap().units().next().unwrap();
            (u.host_translation_disabled(), u.register_span())
        };
        const SMALL: u64 = 16 * 1024;
        const LARGE: u64 = 512 * 1024;
        // 11h and 40h: HATDis is attributes bit 0; PCSup is EFR bit 9 (Tables 98, 102; §2.3).
        for k in [kind::IVHD, kind::IVHD_MIXED] {
            assert_eq!(one(k, 0, 0), (false, SMALL));
            assert_eq!(one(k, 1, 0), (true, SMALL));
            assert_eq!(one(k, 0, 1 << 9), (false, LARGE));
            assert_eq!(
                one(k, 1 << 13, 0),
                (false, SMALL),
                "attribute bits are not counters"
            );
        }
        // 10h: the same field reports features; bit 0 is XTSup, not HATDis, and counters show
        // as PNCounters (16:13) or PNBanks (22:17) (Table 94).
        assert_eq!(one(kind::IVHD_LEGACY, 1, 0), (false, SMALL));
        assert_eq!(one(kind::IVHD_LEGACY, 1 << 13, 0), (false, LARGE));
        assert_eq!(one(kind::IVHD_LEGACY, 1 << 22, 0), (false, LARGE));
        assert_eq!(one(kind::IVHD_LEGACY, 1 << 23 | 1 << 12, 0), (false, SMALL));
    }

    #[test]
    fn malformed_tables_are_refused_at_their_offset() {
        let sel = e4(entry::SELECT, 0x10, 0);
        let unit = |kind: u8, base: u64, entries: &[Vec<u8>]| {
            ivhd(kind, 0, 2, 0x40, base, 0, 0, 0, 0, 0, entries)
        };
        let efr = info::EFR_SUPPORTED;
        // The header.
        assert_eq!(parse(&[0u8; 47]), Err(IvrsError::Short(47)));
        for rev in [0u8, 3, 0xFF] {
            assert_eq!(parse(&table(rev, 0, &[])), Err(IvrsError::Revision(rev)));
        }
        assert!(parse(&table(1, 0, &[])).is_ok());
        // Block headers and lengths.
        let mut t = table(1, 0, &[]);
        t.extend_from_slice(&[0x10, 0, 3]);
        assert_eq!(parse(&t), Err(IvrsError::Block(48)));
        let mut short = unit(kind::IVHD_LEGACY, 0x4000, &[]);
        short[2] = 20;
        assert_eq!(
            parse(&table(1, 0, &[short.clone()])),
            Err(IvrsError::Block(48))
        );
        short[2] = 200;
        assert_eq!(parse(&table(1, 0, &[short])), Err(IvrsError::Block(48)));
        let mut eleven = unit(kind::IVHD, 0x4000, &[]);
        eleven[2] = 36;
        assert_eq!(parse(&table(1, efr, &[eleven])), Err(IvrsError::Block(48)));
        let zero = alloc::vec![0x77, 0, 0, 0];
        assert_eq!(parse(&table(1, 0, &[zero])), Err(IvrsError::Block(48)));
        let mut long_ivmd = ivmd(kind::IVMD_ALL, 0b110, 0, 0, 0, 0x1000, 0x1000);
        long_ivmd.extend_from_slice(&[0; 4]);
        long_ivmd[2] = 36;
        let first = unit(kind::IVHD_LEGACY, 0x4000, core::slice::from_ref(&sel));
        assert_eq!(
            parse(&table(1, 0, &[first.clone(), long_ivmd])),
            Err(IvrsError::Block(48 + first.len()))
        );
        // Block types the header does not allow.
        assert_eq!(
            parse(&table(1, 0, &[unit(kind::IVHD, 0x4000, &[])])),
            Err(IvrsError::BlockType(48))
        );
        assert_eq!(
            parse(&table(1, efr, &[unit(kind::IVHD_MIXED, 0x4000, &[])])),
            Err(IvrsError::BlockType(48))
        );
        assert!(parse(&table(2, efr, &[unit(kind::IVHD_MIXED, 0x4000, &[])])).is_ok());
        // Register bases: zero, misaligned, a repeated type, or a disagreeing description.
        for base in [0, 0x2000, 0xFEB8_1000] {
            assert_eq!(
                parse(&table(1, 0, &[unit(kind::IVHD_LEGACY, base, &[])])),
                Err(IvrsError::RegisterBase(48)),
                "{base:#x}"
            );
        }
        let a = unit(kind::IVHD_LEGACY, 0x4000, &[]);
        assert_eq!(
            parse(&table(1, 0, &[a.clone(), a.clone()])),
            Err(IvrsError::RegisterBase(48 + a.len()))
        );
        let other_id = ivhd(kind::IVHD, 0, 3, 0x40, 0x4000, 0, 0, 0, 0, 0, &[]);
        assert_eq!(
            parse(&table(1, efr, &[a.clone(), other_id])),
            Err(IvrsError::RegisterBase(48 + a.len()))
        );
        assert!(parse(&table(1, efr, &[a.clone(), unit(kind::IVHD, 0x4000, &[])])).is_ok());
        // Device entries.
        let bad_entries: [(Vec<u8>, u8); 4] = [
            (alloc::vec![entry::SELECT, 0x10, 0], kind::IVHD_LEGACY), // truncated 4-byte entry
            (alloc::vec![0x42, 0, 0, 0, 0, 0], kind::IVHD_LEGACY),    // truncated 8-byte entry
            (alloc::vec![0x80, 0, 0, 0], kind::IVHD_MIXED),           // unknown variable type
            (hid(0x10, 0, b"AMDI0040", &[0; 8], 0, b""), kind::IVHD), // HID outside 40h
        ];
        for (e, k) in bad_entries {
            let t = table(2, efr, &[unit(k, 0x4000, &[sel.clone(), e.clone()])]);
            let header = if k == kind::IVHD_LEGACY { 24 } else { 40 };
            assert_eq!(parse(&t), Err(IvrsError::Entry(48 + header + 4)), "{e:?}");
        }
        for (format, uid) in [(0u8, &b"x"[..]), (1, &b""[..]), (3, &b"x"[..])] {
            let e = hid(0x10, 0, b"AMDI0040", &[0; 8], format, uid);
            let t = table(2, efr, &[unit(kind::IVHD_MIXED, 0x4000, &[e])]);
            assert_eq!(
                parse(&t),
                Err(IvrsError::Entry(48 + 40)),
                "{format} {uid:?}"
            );
        }
        let mut cut = hid(0x10, 0, b"AMDI0040", &[0; 8], 2, b"\\_SB.X");
        cut.truncate(25);
        assert_eq!(
            parse(&table(2, efr, &[unit(kind::IVHD_MIXED, 0x4000, &[cut])])),
            Err(IvrsError::Entry(88))
        );
        // Range pairing.
        let start = e4(entry::RANGE_START, 0x20, 0);
        let end = |id| e4(entry::RANGE_END, id, 0);
        let ranges: [(Vec<Vec<u8>>, usize); 4] = [
            (alloc::vec![start.clone(), sel.clone()], 72), // start, then not an end
            (alloc::vec![start.clone()], 72),              // start at the very end
            (alloc::vec![sel.clone(), end(0x30)], 76),     // an end alone
            (alloc::vec![start.clone(), end(0x1F)], 76),   // end below start
        ];
        for (entries, at) in ranges {
            let t = table(1, 0, &[unit(kind::IVHD_LEGACY, 0x4000, &entries)]);
            assert_eq!(parse(&t), Err(IvrsError::Range(at)), "{entries:?}");
        }
        let alias_start = e8(entry::ALIAS_RANGE_START, 0x20, 0, 0, 0x08, 0);
        let t = table(
            1,
            0,
            &[unit(kind::IVHD_LEGACY, 0x4000, &[alias_start, sel.clone()])],
        );
        assert_eq!(parse(&t), Err(IvrsError::Range(72)));
        // "All": twice in one IVHD, or claimed by two IOMMUs of one segment.
        let all = e4(entry::ALL, 0, 0);
        let t = table(
            1,
            0,
            &[unit(kind::IVHD_LEGACY, 0x4000, &[all.clone(), all.clone()])],
        );
        assert_eq!(parse(&t), Err(IvrsError::All(76)));
        let first = unit(kind::IVHD_LEGACY, 0x4000, core::slice::from_ref(&all));
        let second = unit(kind::IVHD_LEGACY, 0x8000, core::slice::from_ref(&all));
        assert_eq!(
            parse(&table(1, 0, &[first.clone(), second])),
            Err(IvrsError::All(48 + first.len()))
        );
        let elsewhere = ivhd(
            kind::IVHD_LEGACY,
            0,
            2,
            0x40,
            0x8000,
            1,
            0,
            0,
            0,
            0,
            core::slice::from_ref(&all),
        );
        assert!(parse(&table(1, 0, &[first.clone(), elsewhere])).is_ok());
        // The same IOMMU described twice may say "all" in both blocks.
        let again = ivhd(kind::IVHD, 0, 2, 0x40, 0x4000, 0, 0, 0, 0, 0, &[all]);
        assert!(parse(&table(1, efr, &[first.clone(), again])).is_ok());
        // Memory ranges.
        let ok =
            |kind, flags, id, last, start, length| ivmd(kind, flags, id, last, 0, start, length);
        assert_eq!(
            parse(&table(
                1,
                0,
                &[ok(kind::IVMD_ALL, 0b110, 0, 0, 0x1000, 0x1000)]
            )),
            Err(IvrsError::Memory(48))
        );
        let bad_ranges = [
            ok(kind::IVMD_ALL, 0b110, 0, 0, u64::MAX - 0xFFF, 0x2000),
            ok(kind::IVMD_RANGE, 0b110, 0x20, 0x1F, 0x1000, 0x1000),
        ];
        for m in bad_ranges {
            let t = table(1, 0, &[first.clone(), m.clone()]);
            assert_eq!(parse(&t), Err(IvrsError::Memory(48 + first.len())), "{m:?}");
        }
        assert!(
            parse(&table(
                1,
                0,
                &[
                    first.clone(),
                    ok(kind::IVMD_DEVICE, 0b1001, 0x20, 0, 0x1000, 0x1000)
                ]
            ))
            .is_ok()
        );
        // Unity with IR = IW = 0: accepted here, and never mapped (review I2).
        assert!(
            parse(&table(
                1,
                0,
                &[
                    first.clone(),
                    ok(kind::IVMD_DEVICE, 0b0001, 0x20, 0, 0x1000, 0x1000)
                ]
            ))
            .is_ok()
        );
        assert!(
            parse(&table(
                1,
                0,
                &[
                    first,
                    ok(kind::IVMD_DEVICE, 0b0000, 0x20, 0, 0x1000, 0x1000)
                ]
            ))
            .is_ok()
        );
    }

    /// Mutated tables never panic the parser or the resolvers, and an accepted table keeps
    /// every invariant `parse` promises (CLAUDE.md §72).
    #[test]
    fn mutations_never_panic_and_accepted_tables_are_consistent() {
        use crate::testing::{Rng, mutate};
        let simple = table(
            1,
            info::EFR_SUPPORTED,
            &[
                ivhd(
                    kind::IVHD_LEGACY,
                    0x30,
                    device_id(0, 5, 0),
                    0x40,
                    0xFED8_0000,
                    0,
                    0,
                    0,
                    0,
                    0,
                    &[
                        e4(entry::RANGE_START, 0, 0),
                        e4(entry::RANGE_END, 0xFF, 0),
                        e8(entry::SPECIAL, 0, 0xD7, 0, 0xA0, 1),
                    ],
                ),
                ivhd(
                    kind::IVHD,
                    0x30,
                    device_id(0, 5, 0),
                    0x40,
                    0xFED8_0000,
                    0,
                    0,
                    0,
                    0x29,
                    0,
                    &[
                        e4(entry::RANGE_START, 0, 0),
                        e4(entry::RANGE_END, 0xFF, 0),
                        e8(entry::SPECIAL, 0, 0xD7, 0, 0xA0, 1),
                    ],
                ),
            ],
        );
        let bases = [simple, platform(), QEMU_IVRS.to_vec()];
        let mut rng = Rng::new(0xA3D1);
        let mut accepted = 0u32;
        for case in 0..crate::testing::cases(12_000) {
            let mut t = bases[case % bases.len()].clone();
            mutate(&mut t, &mut rng);
            if case % 3 != 0 && t.len() >= 36 {
                let len = t.len() as u32;
                t[4..8].copy_from_slice(&len.to_le_bytes());
                crate::dmar::build::seal(&mut t);
            }
            let Ok(r) = parse(&t) else { continue };
            accepted += 1;
            assert!((1..=2).contains(&r.revision), "case {case}");
            let mut bases_seen = Vec::new();
            for u in r.units() {
                assert!(
                    u.base != 0 && u.base.is_multiple_of(REGISTER_ALIGN),
                    "case {case}"
                );
                assert!(
                    !bases_seen.contains(&u.base),
                    "case {case}: one IOMMU twice"
                );
                bases_seen.push(u.base);
                assert!(
                    u.kind != kind::IVHD_LEGACY || u.efr.is_none(),
                    "case {case}"
                );
                let mut alls = 0;
                for e in u.entries() {
                    match e {
                        DeviceEntry::Range { first, last, .. }
                        | DeviceEntry::AliasRange { first, last, .. }
                        | DeviceEntry::ExtendedRange { first, last, .. } => {
                            assert!(first <= last, "case {case}");
                        }
                        DeviceEntry::AcpiHid {
                            uid_format,
                            uid,
                            hid,
                            cid,
                            ..
                        } => {
                            assert_eq!(u.kind, kind::IVHD_MIXED, "case {case}");
                            assert!(
                                uid_format <= 2 && (uid_format == 0) == uid.is_empty(),
                                "case {case}"
                            );
                            assert_eq!((hid.len(), cid.len()), (8, 8), "case {case}");
                        }
                        DeviceEntry::All { .. } => alls += 1,
                        _ => {}
                    }
                }
                assert!(alls <= 1, "case {case}");
                let _ = u.last_device_id();
                let _ = u.special(Special::IoApic, 0);
            }
            for m in r.memory() {
                assert!(m.start.checked_add(m.length).is_some(), "case {case}");
                assert!(
                    m.kind != kind::IVMD_RANGE || m.device_id <= m.last_device_id,
                    "case {case}"
                );
                // Unity with IR = IW = 0 is accepted (review I2): `reserved::from_ivmd`
                // makes it a range never mapped.
                if m.unity() && !m.read() && !m.write() && !m.exclusion() {
                    assert!(
                        !matches!(
                            crate::reserved::from_ivmd(&m),
                            Ok(Some(crate::reserved::Reserved {
                                use_: crate::reserved::Use::Identity(_),
                                ..
                            }))
                        ),
                        "case {case}"
                    );
                }
            }
            for id in [0u16, 0x10, 0xA0, 0x0100, 0x0213, 0x0507, 0xFFFF] {
                let _ = r.unit_for(0, id);
                let _ = r.unit_for(1, id);
            }
            let _ = r.blocks().count();
        }
        assert!(accepted > 1000, "{accepted} accepted");
    }
}
