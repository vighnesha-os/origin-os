//! AMD IOMMU (AMD-Vi) hardware: register and structure encodings (CLAUDE.md §32 AMD IOMMU and
//! interrupt remapping, §58 DMA protection), for the kernel's AMD IOMMU driver.
//!
//! Reference: AMD I/O Virtualization Technology (IOMMU) Specification, publication 48882,
//! Rev 3.11 (April 2026): §2.2.2 (the device table and its 256-bit entries), §2.2.3 (host I/O
//! page tables), §2.2.5 (interrupt remapping tables: the basic 32-bit IRTE, the 128-bit IRTE
//! with guest virtual APIC mode on and GuestMode 0, and §2.2.5.3's 32-bit x2APIC
//! destinations), §2.4 (the command buffer and its commands), §2.5 (the event log and its
//! events), §3.2 (the capability block) and §3.4 (MMIO registers). The research ledger is
//! `docs/research/2026-11-08-amd-vi-iommu.md`.
//!
//! Everything here is pure encoding and decoding, plus a host page-table walker over the
//! same abstract [`TableMemory`] the VT-d walker uses, so all of it is host-tested. Register
//! and table values from hardware are untrusted: decoders never panic, and an encoder refuses
//! (`None`) anything it cannot encode exactly. The access type, the MSI message, the table
//! memory and the IOVA window are shared with [`crate::vtd`], where they were defined first.
//! Pure, no `unsafe`, no allocation.

pub use crate::vtd::{Access, IOVA_BASE, IOVA_END, IovaWindow, MapError, MsiMessage, TableMemory};

/// A 4 KiB page.
pub const PAGE: u64 = 4096;
/// Entries in one page-table page.
pub const ENTRIES: usize = 512;
/// Bits 51:12: a page address in a table entry or a register.
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;

/// MMIO register offsets (§3.4). Every register named here lies in the first 16 KiB.
pub mod reg {
    /// Device Table Base Address.
    pub const DEVICE_TABLE: u64 = 0x0000;
    /// Command Buffer Base Address.
    pub const COMMAND_BUFFER: u64 = 0x0008;
    /// Event Log Base Address.
    pub const EVENT_LOG: u64 = 0x0010;
    /// IOMMU Control.
    pub const CONTROL: u64 = 0x0018;
    /// Exclusion Base and Exclusion Range Limit.
    pub const EXCLUSION_BASE: u64 = 0x0020;
    pub const EXCLUSION_LIMIT: u64 = 0x0028;
    /// Extended Feature Register.
    pub const EFR: u64 = 0x0030;
    /// Extended Feature 2 Register.
    pub const EFR2: u64 = 0x01A0;
    /// Command buffer head and tail pointers.
    pub const COMMAND_HEAD: u64 = 0x2000;
    pub const COMMAND_TAIL: u64 = 0x2008;
    /// Event log head and tail pointers.
    pub const EVENT_HEAD: u64 = 0x2010;
    pub const EVENT_TAIL: u64 = 0x2018;
    /// IOMMU Status.
    pub const STATUS: u64 = 0x2020;
    /// The smallest register set (16 KiB) holds every register above.
    pub const SPAN: u64 = 0x4000;
}

/// IOMMU Control register bits (§3.4.1, MMIO 0018h).
pub mod control {
    pub const IOMMU_EN: u64 = 1 << 0;
    pub const HT_TUN_EN: u64 = 1 << 1;
    pub const EVENT_LOG_EN: u64 = 1 << 2;
    pub const EVENT_INT_EN: u64 = 1 << 3;
    pub const COM_WAIT_INT_EN: u64 = 1 << 4;
    pub const PASS_PW: u64 = 1 << 8;
    pub const RES_PASS_PW: u64 = 1 << 9;
    pub const COHERENT: u64 = 1 << 10;
    pub const ISOC: u64 = 1 << 11;
    pub const CMD_BUF_EN: u64 = 1 << 12;
    /// Guest virtual APIC enable: with GAMEn 000b, IRTEs are 128 bits wide (§2.2.5.2).
    pub const GA_EN: u64 = 1 << 17;
    /// x2APIC enable: 32-bit IRTE destinations (§2.2.5.3; needs GA_EN).
    pub const XT_EN: u64 = 1 << 50;
    /// GAMEn (bits 27:25): 000b, plain remapping. Kept clear.
    pub const GAM_EN: u64 = 0b111 << 25;
}

/// IOMMU Status register bits (§3.4.16, MMIO 2020h).
pub mod status {
    /// RW1C: an event was lost because the log was full.
    pub const EVENT_OVERFLOW: u64 = 1 << 0;
    /// RW1C: an event was written.
    pub const EVENT_LOG_INT: u64 = 1 << 1;
    /// RW1C: a COMPLETION_WAIT with its interrupt bit completed.
    pub const COM_WAIT_INT: u64 = 1 << 2;
    /// Events are being logged.
    pub const EVENT_LOG_RUN: u64 = 1 << 3;
    /// Commands are being fetched.
    pub const CMD_BUF_RUN: u64 = 1 << 4;
}

/// The IOMMU capability header (§3.2, Capability Offset 00h), from configuration space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capability {
    /// Capability ID 0Fh (secure device) and type 011b: this is an IOMMU capability block.
    pub is_iommu: bool,
    pub revision: u8,
    /// IotlbSup: ATS translation requests supported.
    pub iotlb: bool,
    /// NpCache: entries marked not present may be cached, so every change to a page-table
    /// entry needs an invalidation, not only changes to present ones.
    pub not_present_cached: bool,
    /// EFRSup: the Extended Feature Register exists.
    pub efr: bool,
}

impl Capability {
    pub fn decode(header: u32) -> Self {
        Self {
            is_iommu: header & 0xFF == 0x0F && (header >> 16) & 7 == 3,
            revision: ((header >> 19) & 0x1F) as u8,
            iotlb: header & 1 << 24 != 0,
            not_present_cached: header & 1 << 26 != 0,
            efr: header & 1 << 27 != 0,
        }
    }
}

/// The Extended Feature Register (§3.4.1, MMIO 0030h): the features the kernel may use or
/// must know about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Features {
    pub prefetch: bool,
    pub ppr: bool,
    /// XTSup: 32-bit x2APIC destinations in IRTEs.
    pub x2apic: bool,
    pub no_execute: bool,
    pub guest_translation: bool,
    /// IASup: INVALIDATE_IOMMU_ALL.
    pub invalidate_all: bool,
    /// GASup: guest virtual APIC (needed, with XTSup, for 128-bit IRTEs).
    pub guest_apic: bool,
    pub hardware_errors: bool,
    /// PCSup: performance counters (the register set is then 512 KiB).
    pub counters: bool,
    /// The most host page-table levels (HATS: 4, 5 or 6; 0 if reserved).
    pub host_levels: u8,
    /// GAMSup: guest virtual APIC modes.
    pub guest_apic_modes: u8,
}

impl Features {
    pub fn decode(v: u64) -> Self {
        Self {
            prefetch: v & 1 != 0,
            ppr: v & 1 << 1 != 0,
            x2apic: v & 1 << 2 != 0,
            no_execute: v & 1 << 3 != 0,
            guest_translation: v & 1 << 4 != 0,
            invalidate_all: v & 1 << 6 != 0,
            guest_apic: v & 1 << 7 != 0,
            hardware_errors: v & 1 << 8 != 0,
            counters: v & 1 << 9 != 0,
            host_levels: match (v >> 10) & 3 {
                0 => 4,
                1 => 5,
                2 => 6,
                _ => 0,
            },
            guest_apic_modes: ((v >> 21) & 7) as u8,
        }
    }

    /// Whether interrupts can be remapped to 32-bit x2APIC destinations (§2.2.5.3: XTEn and
    /// GAEn on, so XTSup and GASup are both needed).
    pub fn wide_interrupts(&self) -> bool {
        self.x2apic && self.guest_apic
    }
}

/// The interrupts per function the IOMMU can remap (EFR2 NumIntRemapSup, §3.4.13): 512 or
/// 2048.
pub fn interrupts_per_function(efr2: u64) -> u32 {
    if (efr2 >> 8) & 3 == 1 { 2048 } else { 512 }
}

/// Bytes of one device table entry.
pub const DTE_BYTES: u64 = 32;
/// Entries one 4 KiB page of the device table holds.
pub const DTES_PER_PAGE: u32 = (PAGE / DTE_BYTES) as u32;

/// The Device Table Base Address register for a table at `table` with `entries` entries (a
/// multiple of 128 up to 65536, so whole pages; the size field is pages − 1).
pub fn device_table_base(table: u64, entries: u32) -> Option<u64> {
    if table == 0 || table & !ADDRESS != 0 || entries == 0 || entries > 65536 {
        return None;
    }
    if !entries.is_multiple_of(DTES_PER_PAGE) {
        return None;
    }
    Some(table | u64::from(entries / DTES_PER_PAGE - 1))
}

/// Bytes of a command or an event.
pub const RING_ENTRY: u64 = 16;

/// A command buffer or event log base register for a ring of 2^`order` entries at `ring`
/// (order 8–15: 4 KiB to 512 KiB; the ring aligned to 4 KiB).
pub fn ring_base(ring: u64, order: u8) -> Option<u64> {
    if !(8..=15).contains(&order) || ring == 0 || ring & !ADDRESS != 0 {
        return None;
    }
    Some(ring | u64::from(order) << 56)
}

/// The pointer register value (byte offset) of ring entry `index`.
pub fn ring_offset(index: usize) -> u64 {
    index as u64 * RING_ENTRY
}

/// The ring entry a head or tail pointer register names (bits 18:4).
pub fn ring_index(pointer: u64) -> usize {
    ((pointer >> 4) & 0x7FFF) as usize
}

/// How a device's DMA is handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dma {
    /// Every read and write is target aborted and logged as an I/O page fault: V set, TV
    /// clear ("translation information not valid", Table 8 and Table 44), so no request is
    /// translated and none passes untranslated.
    Blocked,
    /// Translated through `levels`-deep host page tables rooted at `root`, read and write
    /// allowed at the top so the page tables decide.
    Translated { root: u64, levels: u8 },
}

/// How a device's fixed and arbitrated interrupts are handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupts {
    /// Target aborted (IV set, IntCtl 00b).
    Blocked,
    /// Remapped through the device's own table at `table` of 2^`order` entries (IV set,
    /// IntCtl 10b).
    Remapped { table: u64, order: u8 },
}

/// A device table entry (§2.2.2.1) as this kernel writes it: host translation only (no guest
/// translation, no ATS, port I/O refused, no exclusion range, page-fault events kept), the
/// firmware's pass bits and SysMgt from the IVRS, and the device's domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dte {
    pub dma: Dma,
    pub domain: u16,
    pub interrupts: Interrupts,
    /// The IVRS DTE setting byte (Table 106): INITPass, EIntPass, NMIPass, SysMgt, Lint0Pass
    /// and Lint1Pass; ATCPermit is ignored (ATS stays off).
    pub setting: u8,
}

/// DTE bits (§2.2.2.1), by 64-bit word.
mod dte {
    pub(super) const V: u64 = 1 << 0;
    pub(super) const TV: u64 = 1 << 1;
    pub(super) const MODE_SHIFT: u32 = 9;
    pub(super) const IR: u64 = 1 << 61;
    pub(super) const IW: u64 = 1 << 62;
    /// Word 1: SysMgt, bits 105:104.
    pub(super) const SYS_MGT_SHIFT: u32 = 40;
    /// Word 2: IV (128), IntTabLen (132:129), the table root (179:134 → 51:6), IntCtl
    /// (189:188), and the pass bits.
    pub(super) const IV: u64 = 1 << 0;
    pub(super) const INT_TAB_LEN_SHIFT: u32 = 1;
    pub(super) const INT_ROOT: u64 = 0x000F_FFFF_FFFF_FFC0;
    pub(super) const INT_CTL_SHIFT: u32 = 60;
    pub(super) const INIT_PASS: u64 = 1 << 56;
    pub(super) const EINT_PASS: u64 = 1 << 57;
    pub(super) const NMI_PASS: u64 = 1 << 58;
    pub(super) const LINT0_PASS: u64 = 1 << 62;
    pub(super) const LINT1_PASS: u64 = 1 << 63;
}

impl Dte {
    /// The four 64-bit words. `None` for a misaligned root (4 KiB) or interrupt table (128
    /// bytes, §2.2.2.1), a depth outside 1–6, or more than 2048 interrupt entries.
    pub fn encode(&self) -> Option<[u64; 4]> {
        let mut w0 = dte::V;
        if let Dma::Translated { root, levels } = self.dma {
            if root == 0 || root & !ADDRESS != 0 || !(1..=6).contains(&levels) {
                return None;
            }
            w0 |= dte::TV | root | u64::from(levels) << dte::MODE_SHIFT | dte::IR | dte::IW;
        }
        let s = u64::from(self.setting);
        let w1 = u64::from(self.domain) | (s >> 4 & 3) << dte::SYS_MGT_SHIFT;
        let mut w2 = dte::IV
            | if s & 1 != 0 { dte::INIT_PASS } else { 0 }
            | if s & 2 != 0 { dte::EINT_PASS } else { 0 }
            | if s & 4 != 0 { dte::NMI_PASS } else { 0 }
            | if s & 0x40 != 0 { dte::LINT0_PASS } else { 0 }
            | if s & 0x80 != 0 { dte::LINT1_PASS } else { 0 };
        if let Interrupts::Remapped { table, order } = self.interrupts {
            if table == 0 || table & 0x7F != 0 || table & !dte::INT_ROOT != 0 || order > 11 {
                return None;
            }
            w2 |= table | u64::from(order) << dte::INT_TAB_LEN_SHIFT | 2 << dte::INT_CTL_SHIFT;
        }
        Some([w0, w1, w2, 0])
    }

    /// The entry back, or `None` for one `encode` never writes (not valid, guest
    /// translation, ATS, port I/O, a reserved or pass-through IntCtl, ...).
    pub fn decode(e: [u64; 4]) -> Option<Self> {
        let [w0, w1, w2, w3] = e;
        // V and TV, Mode, the root, IR and IW only; nothing else in word 0.
        let word0 = dte::V | dte::TV | 7 << dte::MODE_SHIFT | ADDRESS | dte::IR | dte::IW;
        if w0 & dte::V == 0 || w0 & !word0 != 0 {
            return None;
        }
        let levels = ((w0 >> dte::MODE_SHIFT) & 7) as u8;
        let dma = if w0 == dte::V {
            Dma::Blocked
        } else if w0 & dte::TV != 0
            && (1..=6).contains(&levels)
            && w0 & (dte::IR | dte::IW) == dte::IR | dte::IW
            && w0 & ADDRESS != 0
        {
            Dma::Translated {
                root: w0 & ADDRESS,
                levels,
            }
        } else {
            return None;
        };
        let pass =
            dte::INIT_PASS | dte::EINT_PASS | dte::NMI_PASS | dte::LINT0_PASS | dte::LINT1_PASS;
        let table_fields = 0xF << dte::INT_TAB_LEN_SHIFT | dte::INT_ROOT;
        let word2 = dte::IV | table_fields | 3 << dte::INT_CTL_SHIFT | pass;
        if w1 & !(0xFFFF | 3 << dte::SYS_MGT_SHIFT) != 0 || w3 != 0 {
            return None;
        }
        if w2 & dte::IV == 0 || w2 & !word2 != 0 {
            return None;
        }
        let table = w2 & dte::INT_ROOT;
        let order = ((w2 >> dte::INT_TAB_LEN_SHIFT) & 0xF) as u8;
        let interrupts = match (w2 >> dte::INT_CTL_SHIFT) & 3 {
            0 if w2 & table_fields == 0 => Interrupts::Blocked,
            2 if table & 0x7F == 0 && table != 0 && order <= 11 => {
                Interrupts::Remapped { table, order }
            }
            _ => return None,
        };
        let bit = |m: u64, b: u8| if w2 & m != 0 { b } else { 0 };
        let setting = bit(dte::INIT_PASS, 1)
            | bit(dte::EINT_PASS, 2)
            | bit(dte::NMI_PASS, 4)
            | (((w1 >> dte::SYS_MGT_SHIFT) & 3) as u8) << 4
            | bit(dte::LINT0_PASS, 0x40)
            | bit(dte::LINT1_PASS, 0x80);
        Some(Self {
            dma,
            domain: (w1 & 0xFFFF) as u16,
            interrupts,
            setting,
        })
    }
}

/// Host page-table entry bits (§2.2.3).
pub mod pte {
    /// PR: the entry is present.
    pub const PRESENT: u64 = 1 << 0;
    /// NextLevel, bits 11:9: the level of the table a directory entry points to; 0 in a leaf.
    pub const NEXT_LEVEL_SHIFT: u32 = 9;
    pub const NEXT_LEVEL: u64 = 7 << 9;
    /// IR, IW: read and write allowed through this entry (ANDed down the walk).
    pub const READ: u64 = 1 << 61;
    pub const WRITE: u64 = 1 << 62;
}

fn access_bits(access: Access) -> u64 {
    let read = if access.read { pte::READ } else { 0 };
    let write = if access.write { pte::WRITE } else { 0 };
    read | write
}

/// The table index of `iova` at `level` (1 is the leaf level).
fn index(iova: u64, level: u8) -> usize {
    ((iova >> (12 + 9 * u32::from(level - 1))) & 0x1FF) as usize
}

/// Checks the depth and the IOVA's range. Only 3–5 levels are built here (the kernel uses 4;
/// every IOMMU supports at least 4, HATS).
fn check(levels: u8, iova: u64) -> Result<(), MapError> {
    if !(3..=5).contains(&levels) {
        return Err(MapError::Levels);
    }
    if iova >> (12 + 9 * u32::from(levels)) != 0 {
        return Err(MapError::OutOfRange);
    }
    if !iova.is_multiple_of(PAGE) {
        return Err(MapError::Misaligned);
    }
    Ok(())
}

/// Maps the 4 KiB page at `iova` to physical `phys` with `access`, in the `levels`-deep host
/// tables rooted at `root`, creating table pages as needed. Directory entries name the next
/// level (no level is skipped) and allow reading and writing, so the leaf alone decides. An
/// IOVA that is already mapped is refused.
pub fn map(
    mem: &mut impl TableMemory,
    root: u64,
    levels: u8,
    iova: u64,
    phys: u64,
    access: Access,
) -> Result<(), MapError> {
    check(levels, iova)?;
    if phys & !ADDRESS != 0 {
        return Err(MapError::Misaligned);
    }
    if !access.read && !access.write {
        return Err(MapError::NoAccess);
    }
    let mut table = root;
    for level in (2..=levels).rev() {
        let i = index(iova, level);
        let e = mem.read(table, i);
        table = if e & pte::PRESENT != 0 {
            if (e & pte::NEXT_LEVEL) >> pte::NEXT_LEVEL_SHIFT != u64::from(level - 1) {
                // A leaf (a large page) or a skipped level: never made here.
                return Err(MapError::AlreadyMapped);
            }
            e & ADDRESS
        } else {
            let next = mem.new_table().ok_or(MapError::NoMemory)?;
            if next & !ADDRESS != 0 {
                return Err(MapError::Misaligned);
            }
            mem.write(
                table,
                i,
                next | u64::from(level - 1) << pte::NEXT_LEVEL_SHIFT
                    | pte::READ
                    | pte::WRITE
                    | pte::PRESENT,
            );
            next
        };
    }
    let i = index(iova, 1);
    if mem.read(table, i) & pte::PRESENT != 0 {
        return Err(MapError::AlreadyMapped);
    }
    mem.write(table, i, phys | access_bits(access) | pte::PRESENT);
    Ok(())
}

/// Removes the mapping of `iova`; returns the physical page it mapped. The caller must then
/// invalidate the IOMMU's translation cache, and wait, before it reuses the page.
pub fn unmap(
    mem: &mut impl TableMemory,
    root: u64,
    levels: u8,
    iova: u64,
) -> Result<u64, MapError> {
    check(levels, iova)?;
    let mut table = root;
    for level in (2..=levels).rev() {
        let e = mem.read(table, index(iova, level));
        if e & pte::PRESENT == 0
            || (e & pte::NEXT_LEVEL) >> pte::NEXT_LEVEL_SHIFT != u64::from(level - 1)
        {
            return Err(MapError::NotMapped);
        }
        table = e & ADDRESS;
    }
    let i = index(iova, 1);
    let leaf = mem.read(table, i);
    if leaf & pte::PRESENT == 0 {
        return Err(MapError::NotMapped);
    }
    mem.write(table, i, 0);
    Ok(leaf & ADDRESS)
}

/// How the tables translate `iova`, as the hardware would: the physical page and the access
/// every level allows; `None` if an entry is not present or is not one `map` makes.
pub fn translate(
    mem: &mut impl TableMemory,
    root: u64,
    levels: u8,
    iova: u64,
) -> Option<(u64, Access)> {
    check(levels, iova & !(PAGE - 1)).ok()?;
    let (mut read, mut write) = (true, true);
    let mut table = root;
    for level in (1..=levels).rev() {
        let e = mem.read(table, index(iova, level));
        if e & pte::PRESENT == 0
            || (e & pte::NEXT_LEVEL) >> pte::NEXT_LEVEL_SHIFT != u64::from(level - 1)
        {
            return None;
        }
        read &= e & pte::READ != 0;
        write &= e & pte::WRITE != 0;
        if level == 1 {
            return Some((e & ADDRESS, Access { read, write }));
        }
        table = e & ADDRESS;
    }
    None
}

/// An interrupt remapping table entry for a remapped interrupt (§2.2.5): RemapEn set, fixed
/// delivery, physical destination mode, no EOI request, page-fault events kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Irte {
    pub vector: u8,
    /// The destination APIC ID: 32 bits in x2APIC mode, else 8.
    pub destination: u32,
}

/// RemapEn, and the fields `Irte` keeps clear: SupIOPF (1), IntType (4:2), RqEoi (5), DM (6),
/// GuestMode (7).
const IRTE_REMAP: u64 = 1;
const IRTE_CONTROL: u64 = 0xFF;

impl Irte {
    /// The basic 32-bit entry (Figure 15): the vector in bits 23:16, an 8-bit destination in
    /// 15:8. `None` for a vector below 16 or a destination above 255.
    pub fn encode_basic(&self) -> Option<u32> {
        if self.vector < 16 {
            return None;
        }
        let destination = u32::from(u8::try_from(self.destination).ok()?);
        Some(u32::from(self.vector) << 16 | destination << 8 | IRTE_REMAP as u32)
    }

    /// The 128-bit entry with GuestMode 0 (Figures 17 and 19): the vector in bits 71:64; with
    /// `x2apic` (XTEn), destination bits 23:0 in 31:8 and 31:24 in 127:120, else an 8-bit
    /// destination in 15:8. `None` for a vector below 16, or an 8-bit destination above 255.
    pub fn encode_wide(&self, x2apic: bool) -> Option<[u64; 2]> {
        if self.vector < 16 {
            return None;
        }
        let d = u64::from(self.destination);
        let (lo_dest, hi_dest) = if x2apic {
            ((d & 0xFF_FFFF) << 8, (d >> 24) << 56)
        } else {
            (u64::from(u8::try_from(self.destination).ok()?) << 8, 0)
        };
        Some([lo_dest | IRTE_REMAP, hi_dest | u64::from(self.vector)])
    }

    /// A basic entry back; `None` unless it is one `encode_basic` writes (remapped, fixed,
    /// physical, GuestMode 0, reserved bits clear).
    pub fn decode_basic(e: u32) -> Option<Self> {
        if u64::from(e) & IRTE_CONTROL != IRTE_REMAP || e >> 24 != 0 {
            return None;
        }
        Some(Self {
            vector: (e >> 16) as u8,
            destination: (e >> 8) & 0xFF,
        })
    }

    /// A 128-bit entry back; `None` unless it is one `encode_wide` writes.
    pub fn decode_wide(e: [u64; 2], x2apic: bool) -> Option<Self> {
        let [lo, hi] = e;
        if lo & IRTE_CONTROL != IRTE_REMAP || lo >> 32 != 0 || hi & 0x00FF_FFFF_FFFF_FF00 != 0 {
            return None;
        }
        let destination = if x2apic {
            ((lo >> 8) & 0xFF_FFFF | (hi >> 56) << 24) as u32
        } else {
            if lo >> 16 != 0 || hi >> 56 != 0 {
                return None;
            }
            ((lo >> 8) & 0xFF) as u32
        };
        Some(Self {
            vector: hi as u8,
            destination,
        })
    }
}

/// The interrupt address range's fixed head, `FEEh` in address bits 31:20.
const INTERRUPT_HEAD: u64 = 0xFEE0_0000;

/// IRTEs reachable with a fixed message type: data bits 10:8 carry the message type (MT 000b,
/// fixed) as well as the top of the index (Table 19, Figure 14), so indexes stay below 256.
pub const FIXED_INDEXES: u16 = 256;

/// The message that selects IRTE `index` of the sending device's own table: a plain
/// fixed-type MSI to the interrupt range, destination 0, data = the index. The IOMMU reads
/// the vector and destination from the IRTE and ignores the message's. `None` at or above
/// [`FIXED_INDEXES`].
pub fn remapped_msi(index: u16) -> Option<MsiMessage> {
    (index < FIXED_INDEXES).then_some(MsiMessage {
        address: INTERRUPT_HEAD,
        data: u32::from(index),
    })
}

/// The IRTE index the IOMMU takes from `m` (data bits 10:0, of which 10:8 are also the
/// message type); `None` if `m` is not an MSI to the interrupt range, is level-triggered, or
/// has a message type other than fixed (000b) or arbitrated (001b), which the IOMMU handles
/// by the DTE's pass bits rather than the table.
pub fn msi_index(m: MsiMessage) -> Option<u16> {
    if m.address >> 32 != 0 || m.address & 0xFFF0_0000 != INTERRUPT_HEAD {
        return None;
    }
    if m.data >> 11 != 0 || (m.data >> 8) & 7 > 1 {
        return None;
    }
    Some((m.data & 0x1FF) as u16)
}

/// How much of the address space an IOMMU page invalidation covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pages {
    /// The 4 KiB page at the address (S 0); leaf entries only (PDE 0).
    One,
    /// Every page of the domain, directory entries included (S 1, PDE 1, address
    /// 7FFF_FFFF_FFFF_F000h, §2.4.3).
    All,
}

/// A command (§2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// COMPLETION_WAIT (01h): once every earlier command has completed, write `data` to the
    /// 8-byte aligned `store`; with `flush`, later commands wait for it too.
    CompletionWait { store: u64, data: u64, flush: bool },
    /// INVALIDATE_DEVTAB_ENTRY (02h).
    InvalidateDevice { device: u16 },
    /// INVALIDATE_IOMMU_PAGES (03h), host translations (GN 0, PASID 0).
    InvalidatePages {
        domain: u16,
        address: u64,
        pages: Pages,
    },
    /// INVALIDATE_INTERRUPT_TABLE (05h).
    InvalidateInterrupts { device: u16 },
    /// INVALIDATE_IOMMU_ALL (08h); only where EFR IASup is set.
    InvalidateAll,
}

/// Every page of a domain, as INVALIDATE_IOMMU_PAGES spells it with S set.
const ALL_PAGES: u64 = 0x7FFF_FFFF_FFFF_F000;

impl Command {
    /// The 128-bit command as two little-endian words (operand dwords 0–1, then 2–3), the
    /// opcode in bits 63:60 of the first. `None` for a misaligned store or page address.
    pub fn encode(&self) -> Option<[u64; 2]> {
        let op = |code: u64| code << 60;
        Some(match *self {
            Self::CompletionWait { store, data, flush } => {
                if store == 0 || store & 7 != 0 || store >> 52 != 0 {
                    return None;
                }
                let s = 1u64;
                let f = if flush { 1 << 2 } else { 0 };
                [store | f | s | op(1), data]
            }
            Self::InvalidateDevice { device } => [u64::from(device) | op(2), 0],
            Self::InvalidatePages {
                domain,
                address,
                pages,
            } => {
                let hi = match pages {
                    Pages::One => {
                        if !address.is_multiple_of(PAGE) {
                            return None;
                        }
                        address
                    }
                    Pages::All => ALL_PAGES | 0b11,
                };
                [u64::from(domain) << 32 | op(3), hi]
            }
            Self::InvalidateInterrupts { device } => [u64::from(device) | op(5), 0],
            Self::InvalidateAll => [op(8), 0],
        })
    }
}

/// Event codes (§2.5, Table 42).
pub mod event {
    pub const ILLEGAL_DEV_TABLE_ENTRY: u8 = 1;
    pub const IO_PAGE_FAULT: u8 = 2;
    pub const DEV_TAB_HARDWARE_ERROR: u8 = 3;
    pub const PAGE_TAB_HARDWARE_ERROR: u8 = 4;
    pub const ILLEGAL_COMMAND_ERROR: u8 = 5;
    pub const COMMAND_HARDWARE_ERROR: u8 = 6;
    pub const IOTLB_INV_TIMEOUT: u8 = 7;
    pub const INVALID_DEVICE_REQUEST: u8 = 8;
}

/// One event log entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub code: u8,
    /// The DeviceID the event is about (0 for command errors).
    pub device: u16,
    /// For page faults: the DomainID (or PASID when GN is set).
    pub domain: u16,
    /// Flag bits 24:16 of the second dword: TR (24), RZ (23), PE (22), RW (21), PR (20), I
    /// (19), US (18), NX (17), GN (16).
    flags: u16,
    /// The device address (page faults), or the system address of the failing access or
    /// command.
    pub address: u64,
}

impl Event {
    /// The entry back; `None` for an all-zero (never written) entry or the reserved code 0.
    pub fn decode(lo: u64, hi: u64) -> Option<Self> {
        let code = (lo >> 60) as u8;
        if code == 0 {
            return None;
        }
        let address = if code == event::ILLEGAL_DEV_TABLE_ENTRY {
            hi & !3
        } else {
            hi
        };
        Some(Self {
            code,
            device: (lo & 0xFFFF) as u16,
            domain: ((lo >> 32) & 0xFFFF) as u16,
            flags: ((lo >> 48) & 0x1FF) as u16,
            address,
        })
    }

    /// I: the request was an interrupt.
    pub fn interrupt(&self) -> bool {
        self.flags & 1 << 3 != 0
    }

    /// PR: the page (or the IRTE) was present, so the fault is about permissions or format.
    pub fn present(&self) -> bool {
        self.flags & 1 << 4 != 0
    }

    /// RW: a write (meaningful only when present, for memory requests).
    pub fn write(&self) -> bool {
        self.flags & 1 << 5 != 0
    }

    /// PE: the device lacked permission.
    pub fn permission(&self) -> bool {
        self.flags & 1 << 6 != 0
    }

    /// RZ: a reserved bit was set (else, with PR, a bad level encoding).
    pub fn reserved_bit(&self) -> bool {
        self.flags & 1 << 7 != 0
    }

    /// TR: the request was an ATS translation request.
    pub fn translation(&self) -> bool {
        self.flags & 1 << 8 != 0
    }
}

/// What an event code means.
pub fn event_name(code: u8) -> &'static str {
    match code {
        event::ILLEGAL_DEV_TABLE_ENTRY => "illegal device table entry",
        event::IO_PAGE_FAULT => "I/O page fault",
        event::DEV_TAB_HARDWARE_ERROR => "device table hardware error",
        event::PAGE_TAB_HARDWARE_ERROR => "page table hardware error",
        event::ILLEGAL_COMMAND_ERROR => "illegal command",
        event::COMMAND_HARDWARE_ERROR => "command buffer hardware error",
        event::IOTLB_INV_TIMEOUT => "IOTLB invalidation timeout",
        event::INVALID_DEVICE_REQUEST => "invalid device request",
        9 => "invalid PPR request or event counter zero",
        _ => "reserved event",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn registers_and_capabilities_decode() {
        // QEMU's capability header (ID 0Fh, type 3, revision 1, EFRSup, NpCache, IotlbSup,
        // HtTunnel) and an EFR with XTSup, GASup, IASup and HATS 4 levels.
        let c =
            Capability::decode(0x0F | 3 << 16 | 1 << 19 | 1 << 24 | 1 << 25 | 1 << 26 | 1 << 27);
        assert!(c.is_iommu && c.iotlb && c.not_present_cached && c.efr);
        assert_eq!(c.revision, 1);
        assert!(!Capability::decode(0x05 | 3 << 16).is_iommu);
        assert!(!Capability::decode(0x0F | 2 << 16).is_iommu);
        let f = Features::decode(1 << 2 | 1 << 6 | 1 << 7 | 0b01 << 10 | 0b001 << 21);
        assert!(f.x2apic && f.invalidate_all && f.guest_apic && f.wide_interrupts());
        assert!(!f.prefetch && !f.ppr && !f.counters);
        assert_eq!((f.host_levels, f.guest_apic_modes), (5, 1));
        assert_eq!(Features::decode(0b11 << 10).host_levels, 0);
        assert!(!Features::decode(1 << 2).wide_interrupts());
        assert_eq!(interrupts_per_function(0), 512);
        assert_eq!(interrupts_per_function(1 << 8), 2048);
        assert_eq!(interrupts_per_function(3 << 8), 512);
        // Base registers.
        assert_eq!(device_table_base(0x10_0000, 256), Some(0x10_0001));
        assert_eq!(device_table_base(0x20_0000, 65536), Some(0x20_01FF));
        for (t, n) in [
            (0, 128),
            (0x1800, 128),
            (0x1000, 100),
            (0x1000, 0),
            (0x1000, 65536 + 128),
        ] {
            assert_eq!(device_table_base(t, n), None, "{t:#x} {n}");
        }
        assert_eq!(ring_base(0x5000, 8), Some(0x0800_0000_0000_5000));
        assert_eq!(ring_base(0x5000, 15), Some(0x0F00_0000_0000_5000));
        assert_eq!(ring_base(0x5000, 7), None);
        assert_eq!(ring_base(0x5008, 8), None);
        assert_eq!(
            (
                ring_offset(3),
                ring_index(0x30),
                ring_index(0x8_0000 | 0x30)
            ),
            (0x30, 3, 3)
        );
    }

    #[test]
    fn device_table_entries_round_trip() {
        let blocked = Dte {
            dma: Dma::Blocked,
            domain: 0,
            interrupts: Interrupts::Blocked,
            setting: 0,
        };
        // V alone (TV clear: nothing is translated, nothing passes); IV with IntCtl 00b.
        assert_eq!(blocked.encode(), Some([1, 0, 1, 0]));
        let attached = Dte {
            dma: Dma::Translated {
                root: 0x7_5000,
                levels: 4,
            },
            domain: 7,
            interrupts: Interrupts::Remapped {
                table: 0x8_1000,
                order: 8,
            },
            setting: 0xD7,
        };
        let w = attached.encode().unwrap();
        assert_eq!(w[0], 0x6000_0000_0007_5803);
        assert_eq!(w[1], 7 | 1 << 40);
        assert_eq!(
            w[2],
            1 | 8 << 1 | 0x8_1000 | 2 << 60 | 1 << 56 | 1 << 57 | 1 << 58 | 1 << 62 | 1 << 63
        );
        assert_eq!(w[3], 0);
        for d in [blocked, attached] {
            assert_eq!(Dte::decode(d.encode().unwrap()), Some(d));
        }
        // Refusals.
        let bad = [
            Dte {
                dma: Dma::Translated {
                    root: 0x7_5800,
                    levels: 4,
                },
                ..attached
            },
            Dte {
                dma: Dma::Translated { root: 0, levels: 4 },
                ..attached
            },
            Dte {
                dma: Dma::Translated {
                    root: 0x7_5000,
                    levels: 7,
                },
                ..attached
            },
            Dte {
                interrupts: Interrupts::Remapped {
                    table: 0x8_1040,
                    order: 8,
                },
                ..attached
            },
            Dte {
                interrupts: Interrupts::Remapped {
                    table: 0x8_1000,
                    order: 12,
                },
                ..attached
            },
        ];
        for d in bad {
            assert_eq!(d.encode(), None, "{d:?}");
        }
        // Entries this kernel never writes do not decode.
        let w = attached.encode().unwrap();
        for e in [
            [0, w[1], w[2], w[3]],                 // not valid
            [w[0] & !2, w[1], w[2], w[3]],         // TV clear with a root: neither form
            [3, w[1], w[2], w[3]],                 // TV set, Mode 0: untranslated, never written
            [w[0] | 1 << 55, w[1], w[2], w[3]],    // guest translation
            [w[0] & !(1 << 62), w[1], w[2], w[3]], // write not allowed at the top
            [w[0], w[1] | 1 << 32, w[2], w[3]],    // ATS (I)
            [w[0], w[1], w[2] & !1, w[3]],         // IV clear
            [w[0], w[1], w[2] ^ 3 << 60, w[3]],    // IntCtl 01b, pass-through
            [w[0], w[1], w[2], 1],                 // guest fields
            [w[0], w[1], w[2] | 1 << 5, w[3]],     // IG
            [w[0] | 1 << 2, w[1], w[2], w[3]],     // a reserved bit in word 0
            [w[0], w[1], w[2] | 0x40, w[3]],       // a table not 128-byte aligned
        ] {
            assert_eq!(Dte::decode(e), None, "{e:x?}");
        }
    }

    /// Table memory for the walker tests: pages in a map, handing out fresh pages from a
    /// counter, with a page budget.
    struct Model {
        pages: BTreeMap<u64, [u64; ENTRIES]>,
        next: u64,
        budget: usize,
    }

    impl Model {
        fn new(budget: usize) -> (Self, u64) {
            let mut m = Self {
                pages: BTreeMap::new(),
                next: 0x10_0000,
                budget,
            };
            let root = m.new_table().unwrap();
            (m, root)
        }
    }

    impl TableMemory for Model {
        fn read(&mut self, table: u64, index: usize) -> u64 {
            self.pages.get(&table).map_or(0, |p| p[index])
        }

        fn write(&mut self, table: u64, index: usize, value: u64) {
            self.pages.entry(table).or_insert([0; ENTRIES])[index] = value;
        }

        fn new_table(&mut self) -> Option<u64> {
            if self.pages.len() >= self.budget {
                return None;
            }
            let t = self.next;
            self.next += PAGE;
            self.pages.insert(t, [0; ENTRIES]);
            Some(t)
        }
    }

    #[test]
    fn the_walker_maps_translates_and_unmaps() {
        let (mut m, root) = Model::new(16);
        let iova = IOVA_BASE + 0x1_2000;
        assert_eq!(
            map(&mut m, root, 4, iova, 0x7_5000, Access::READ_WRITE),
            Ok(())
        );
        // Four levels: the root's entry names level 3, then 2, then 1; the leaf has NextLevel 0.
        let l4 = m.read(root, index(iova, 4));
        assert_eq!(
            l4 & (pte::PRESENT | pte::READ | pte::WRITE | pte::NEXT_LEVEL),
            pte::PRESENT | pte::READ | pte::WRITE | 3 << 9
        );
        assert_eq!(m.pages.len(), 4);
        assert_eq!(
            translate(&mut m, root, 4, iova),
            Some((0x7_5000, Access::READ_WRITE))
        );
        assert_eq!(
            translate(&mut m, root, 4, iova + 0x123),
            Some((0x7_5000, Access::READ_WRITE))
        );
        // A neighbour shares every table; a read-only page reads back so.
        assert_eq!(
            map(&mut m, root, 4, iova + PAGE, 0x9_9000, Access::READ),
            Ok(())
        );
        assert_eq!(m.pages.len(), 4);
        assert_eq!(
            translate(&mut m, root, 4, iova + PAGE),
            Some((0x9_9000, Access::READ))
        );
        assert_eq!(
            map(&mut m, root, 4, iova, 0x8_8000, Access::READ_WRITE),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(unmap(&mut m, root, 4, iova), Ok(0x7_5000));
        assert_eq!(translate(&mut m, root, 4, iova), None);
        assert_eq!(unmap(&mut m, root, 4, iova), Err(MapError::NotMapped));
        assert_eq!(map(&mut m, root, 4, iova, 0x8_8000, Access::WRITE), Ok(()));
        assert_eq!(
            translate(&mut m, root, 4, iova),
            Some((0x8_8000, Access::WRITE))
        );
        // Refusals.
        assert_eq!(
            map(&mut m, root, 4, iova + 1, 0, Access::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            map(&mut m, root, 4, 1 << 48, 0, Access::READ),
            Err(MapError::OutOfRange)
        );
        assert_eq!(
            map(&mut m, root, 6, iova, 0, Access::READ),
            Err(MapError::Levels)
        );
        assert_eq!(
            map(&mut m, root, 4, iova + 2 * PAGE, 0x1001, Access::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            map(
                &mut m,
                root,
                4,
                iova + 2 * PAGE,
                0x1000,
                Access {
                    read: false,
                    write: false
                }
            ),
            Err(MapError::NoAccess)
        );
        // A directory entry that names a skipped level is not this walker's: it neither
        // translates nor maps through it.
        let i = index(iova, 4);
        let e = m.read(root, i);
        m.write(root, i, (e & !pte::NEXT_LEVEL) | 2 << 9);
        assert_eq!(translate(&mut m, root, 4, iova), None);
        assert_eq!(
            map(&mut m, root, 4, iova + 3 * PAGE, 0x1000, Access::READ),
            Err(MapError::AlreadyMapped)
        );
        // Table pages run out.
        let (mut small, root) = Model::new(2);
        assert_eq!(
            map(&mut small, root, 4, iova, 0x1000, Access::READ),
            Err(MapError::NoMemory)
        );
    }

    /// A random sequence of maps and unmaps against a plain map from IOVA to page: the walker
    /// always agrees with it (CLAUDE.md §72, model-based testing).
    #[test]
    fn the_walker_agrees_with_a_model() {
        use crate::testing::Rng;
        let (mut m, root) = Model::new(4096);
        let mut model = BTreeMap::new();
        let mut rng = Rng::new(0xA11D);
        for step in 0..crate::testing::cases(20_000) {
            // IOVAs over four level-1 tables, in two level-2 tables.
            let iova =
                IOVA_BASE + (rng.below(4) as u64) * (1 << 30) / 2 + (rng.below(64) as u64) * PAGE;
            let iova = iova & !(PAGE - 1);
            if rng.below(2) == 0 {
                let phys = (rng.below(1 << 20) as u64 + 1) * PAGE;
                let r = map(&mut m, root, 4, iova, phys, Access::READ_WRITE);
                match model.entry(iova) {
                    std::collections::btree_map::Entry::Occupied(_) => {
                        assert_eq!(r, Err(MapError::AlreadyMapped), "step {step}");
                    }
                    std::collections::btree_map::Entry::Vacant(e) => {
                        assert_eq!(r, Ok(()), "step {step}");
                        e.insert(phys);
                    }
                }
            } else {
                let r = unmap(&mut m, root, 4, iova);
                assert_eq!(r.ok(), model.remove(&iova), "step {step}");
            }
            assert_eq!(
                translate(&mut m, root, 4, iova).map(|(p, _)| p),
                model.get(&iova).copied(),
                "step {step}"
            );
        }
    }

    #[test]
    fn interrupt_entries_and_messages() {
        let e = Irte {
            vector: 0x41,
            destination: 3,
        };
        assert_eq!(e.encode_basic(), Some(0x0041_0301));
        assert_eq!(Irte::decode_basic(0x0041_0301), Some(e));
        assert_eq!(e.encode_wide(false), Some([0x301, 0x41]));
        assert_eq!(e.encode_wide(true), Some([0x301, 0x41]));
        let wide = Irte {
            vector: 0x41,
            destination: 0x1234_5678,
        };
        assert_eq!(
            wide.encode_wide(true),
            Some([0x3456_7801, 0x1200_0000_0000_0041])
        );
        assert_eq!(
            Irte::decode_wide([0x3456_7801, 0x1200_0000_0000_0041], true),
            Some(wide)
        );
        assert_eq!(Irte::decode_wide([0x301, 0x41], false), Some(e));
        // Refusals: reserved vectors, an 8-bit destination above 255.
        assert_eq!(Irte { vector: 15, ..e }.encode_basic(), None);
        assert_eq!(Irte { vector: 15, ..e }.encode_wide(true), None);
        assert_eq!(
            Irte {
                destination: 300,
                ..e
            }
            .encode_basic(),
            None
        );
        assert_eq!(
            Irte {
                destination: 300,
                ..e
            }
            .encode_wide(false),
            None
        );
        assert_eq!(
            Irte {
                destination: 300,
                ..e
            }
            .encode_wide(true),
            Some([300 << 8 | 1, 0x41])
        );
        // Not remapped, guest mode, logical destination, arbitrated, or reserved bits set.
        for bad in [
            0x0041_0300u32,
            0x0041_0381,
            0x0041_0341,
            0x0041_0305,
            0x0141_0301,
        ] {
            assert_eq!(Irte::decode_basic(bad), None, "{bad:#x}");
        }
        for bad in [
            [0x300, 0x41],
            [0x381, 0x41],
            [0x301, 0x41 | 1 << 8],
            [0x301 | 1 << 32, 0x41],
        ] {
            assert_eq!(Irte::decode_wide(bad, true), None, "{bad:x?}");
        }
        // Without x2APIC, destination bits above 7 are reserved.
        assert_eq!(Irte::decode_wide([0x1_0301, 0x41], false), None);
        // Messages.
        assert_eq!(
            remapped_msi(5),
            Some(MsiMessage {
                address: 0xFEE0_0000,
                data: 5
            })
        );
        assert_eq!(remapped_msi(256), None);
        for i in [0u16, 1, 255] {
            assert_eq!(remapped_msi(i).and_then(msi_index), Some(i));
        }
        assert_eq!(
            msi_index(MsiMessage {
                address: 0xFEE0_1000,
                data: 0x1FF
            }),
            Some(0x1FF)
        );
        for bad in [
            MsiMessage {
                address: 0xFED0_0000,
                data: 1,
            },
            MsiMessage {
                address: 1 << 32 | 0xFEE0_0000,
                data: 1,
            },
            MsiMessage {
                address: 0xFEE0_0000,
                data: 1 << 15 | 1,
            },
            MsiMessage {
                address: 0xFEE0_0000,
                data: 0x800,
            },
            MsiMessage {
                address: 0xFEE0_0000,
                data: 0x200,
            },
        ] {
            assert_eq!(msi_index(bad), None, "{bad:x?}");
        }
    }

    #[test]
    fn commands_encode() {
        assert_eq!(
            Command::CompletionWait {
                store: 0x7_5008,
                data: 0xDEAD_BEEF_0000_0001,
                flush: true
            }
            .encode(),
            Some([0x1000_0000_0007_500D, 0xDEAD_BEEF_0000_0001])
        );
        assert_eq!(
            Command::CompletionWait {
                store: 0x12_3456_7000,
                data: 2,
                flush: false
            }
            .encode(),
            Some([0x1000_0012_3456_7001, 2])
        );
        for store in [0, 0x7_5004, 1 << 52] {
            assert_eq!(
                Command::CompletionWait {
                    store,
                    data: 1,
                    flush: false
                }
                .encode(),
                None,
                "{store:#x}"
            );
        }
        assert_eq!(
            Command::InvalidateDevice { device: 0x0010 }.encode(),
            Some([0x2000_0000_0000_0010, 0])
        );
        assert_eq!(
            Command::InvalidatePages {
                domain: 7,
                address: 0x4001_2000,
                pages: Pages::One
            }
            .encode(),
            Some([0x3000_0007_0000_0000, 0x4001_2000])
        );
        assert_eq!(
            Command::InvalidatePages {
                domain: 7,
                address: 0,
                pages: Pages::All
            }
            .encode(),
            Some([0x3000_0007_0000_0000, 0x7FFF_FFFF_FFFF_F003])
        );
        assert_eq!(
            Command::InvalidatePages {
                domain: 7,
                address: 0x4001_2800,
                pages: Pages::One
            }
            .encode(),
            None
        );
        assert_eq!(
            Command::InvalidateInterrupts { device: 0x00A0 }.encode(),
            Some([0x5000_0000_0000_00A0, 0])
        );
        assert_eq!(
            Command::InvalidateAll.encode(),
            Some([0x8000_0000_0000_0000, 0])
        );
    }

    #[test]
    fn events_decode() {
        // An I/O page fault: DeviceID 0x0010, domain 1, a write to a not-present page.
        let lo = 0x0010 | 1 << 32 | (1u64 << 5) << 48 | 2 << 60;
        let e = Event::decode(lo, 0x4001_2000).unwrap();
        assert_eq!(
            (e.code, e.device, e.domain, e.address),
            (2, 0x0010, 1, 0x4001_2000)
        );
        assert!(
            e.write()
                && !e.present()
                && !e.interrupt()
                && !e.permission()
                && !e.reserved_bit()
                && !e.translation()
        );
        // An interrupt blocked by an IRTE with RemapEn clear: I set, PR clear.
        let irq = Event::decode(0x0010 | (1u64 << 3) << 48 | 2 << 60, 0xFEE0_0000).unwrap();
        assert!(irq.interrupt() && !irq.present());
        // A permission fault on a present page, a reserved bit, a translation request.
        let pe = Event::decode((1u64 << 4 | 1 << 6 | 1 << 7 | 1 << 8) << 48 | 2 << 60, 0).unwrap();
        assert!(pe.present() && pe.permission() && pe.reserved_bit() && pe.translation());
        // An illegal DTE: the address's low two bits are reserved.
        let dte = Event::decode(0x0018 | 1 << 60, 0x1000_0003).unwrap();
        assert_eq!((dte.code, dte.device, dte.address), (1, 0x18, 0x1000_0000));
        assert_eq!(Event::decode(0, 0), None);
        assert_eq!(Event::decode(0x1234, 0x5678), None);
        for code in 0..16 {
            assert!(!event_name(code).is_empty());
        }
        // Random entries never panic.
        let mut rng = crate::testing::Rng::new(0xE7);
        for _ in 0..crate::testing::cases(20_000) {
            if let Some(e) = Event::decode(rng.next(), rng.next()) {
                let _ = (e.interrupt(), e.present(), e.write(), event_name(e.code));
            }
        }
    }
}
