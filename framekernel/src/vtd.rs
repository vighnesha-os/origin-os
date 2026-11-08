//! Intel VT-d remapping hardware: register and structure encodings (CLAUDE.md §32 Intel
//! VT-d and interrupt remapping, §58 DMA protection), for the kernel's `iommu` driver.
//!
//! Reference: Intel Virtualization Technology for Directed I/O Architecture Specification,
//! Rev 5.20 (D51397-019, 2026-05-14): chapter 3 (DMA remapping: legacy-mode root and context
//! entries, second-level translation), chapter 5 (interrupt remapping: the remappable message
//! format and the IRTE), chapter 6 (invalidation: queued-invalidation descriptors), chapter 7
//! (fault recording and the fault reasons), chapter 9 (structure formats) and chapter 10
//! (registers). The research ledger is `docs/research/2026-11-08-vt-d-iommu.md`.
//!
//! Everything here is pure encoding and decoding, plus a second-level page-table walker over
//! an abstract [`TableMemory`], so all of it is host-tested. Register values come from
//! hardware and are untrusted: decoders never panic, and an encoder refuses (`None`) anything
//! it cannot encode exactly. Pure, no `unsafe`, no allocation.

/// Register offsets in a remapping unit's register set (chapter 10).
pub mod reg {
    /// Version (32-bit).
    pub const VER: u64 = 0x00;
    /// Capability (64-bit).
    pub const CAP: u64 = 0x08;
    /// Extended capability (64-bit).
    pub const ECAP: u64 = 0x10;
    /// Global command (32-bit, write).
    pub const GCMD: u64 = 0x18;
    /// Global status (32-bit, read).
    pub const GSTS: u64 = 0x1C;
    /// Root table address (64-bit).
    pub const RTADDR: u64 = 0x20;
    /// Fault status (32-bit).
    pub const FSTS: u64 = 0x34;
    /// Fault event control (32-bit).
    pub const FECTL: u64 = 0x38;
    /// Invalidation queue head (64-bit).
    pub const IQH: u64 = 0x80;
    /// Invalidation queue tail (64-bit).
    pub const IQT: u64 = 0x88;
    /// Invalidation queue address (64-bit).
    pub const IQA: u64 = 0x90;
    /// Interrupt remapping table address (64-bit).
    pub const IRTA: u64 = 0xB8;
}

/// Global command bits; each has its status bit at the same position in GSTS.
pub mod gcmd {
    /// Translation enable (TES).
    pub const TE: u32 = 1 << 31;
    /// Set root table pointer (RTPS), one-shot.
    pub const SRTP: u32 = 1 << 30;
    /// Set fault log (FLS), one-shot.
    pub const SFL: u32 = 1 << 29;
    /// Enable advanced fault logging (AFLS).
    pub const EAFL: u32 = 1 << 28;
    /// Write buffer flush (WBFS), one-shot.
    pub const WBF: u32 = 1 << 27;
    /// Queued invalidation enable (QIES).
    pub const QIE: u32 = 1 << 26;
    /// Interrupt remapping enable (IRES).
    pub const IRE: u32 = 1 << 25;
    /// Set interrupt remap table pointer (IRTPS), one-shot.
    pub const SIRTP: u32 = 1 << 24;
    /// Compatibility-format interrupts allowed (CFIS).
    pub const CFI: u32 = 1 << 23;
}

/// The GSTS bits that report one-shot commands; a GCMD write must not echo them back, or it
/// would repeat the command.
const ONE_SHOT: u32 = gcmd::SRTP | gcmd::SFL | gcmd::WBF | gcmd::SIRTP;

/// The GCMD value that issues one command, given the current GSTS: every enable that is on
/// stays on, the one-shot status bits are dropped, then `set` is set and `clear` cleared
/// (chapter 10, the global command register: software reads GSTS, masks the one-shot bits
/// and writes the result with its command).
pub fn gcmd(gsts: u32, set: u32, clear: u32) -> u32 {
    (gsts & !ONE_SHOT & !clear) | set
}

/// The version register: (major, minor).
pub fn version(ver: u32) -> (u8, u8) {
    (((ver >> 4) & 0xF) as u8, (ver & 0xF) as u8)
}

/// The capability register (chapter 10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capability {
    /// Bits of domain ID supported (4 + 2 × ND; ND 7 is reserved and decodes as 0).
    pub domain_bits: u8,
    /// RWBF: software must flush the write buffer after changing remapping structures.
    pub write_buffer_flush: bool,
    /// CM: caching mode. Not-present entries may be cached, so making an entry present
    /// needs an invalidation too.
    pub caching_mode: bool,
    /// SAGAW: second-level table depths supported. Bit 1: 3 levels (39-bit); bit 2: 4
    /// levels (48-bit); bit 3: 5 levels (57-bit).
    pub sagaw: u8,
    /// MGAW: the largest input address width, in bits.
    pub mgaw: u8,
    /// FRO: byte offset of the first fault-recording register.
    pub fault_offset: u64,
    /// NFR: how many fault-recording registers there are.
    pub fault_records: u16,
    /// SLLPS: second-level large pages (bit 0: 2 MiB, bit 1: 1 GiB).
    pub large_pages: u8,
    /// PSI: page-selective IOTLB invalidation.
    pub page_invalidation: bool,
    /// MAMV: the largest address mask a page-selective invalidation takes.
    pub max_address_mask: u8,
    /// DWD: invalidations can drain writes.
    pub drain_writes: bool,
    /// DRD: invalidations can drain reads.
    pub drain_reads: bool,
    /// ESIRTPS: SIRTP also invalidates the interrupt entry cache.
    pub enhanced_sirtp: bool,
    /// ESRTPS: SRTP also invalidates every translation cache.
    pub enhanced_srtp: bool,
}

impl Capability {
    pub fn decode(v: u64) -> Self {
        let nd = (v & 7) as u8;
        Self {
            domain_bits: if nd == 7 { 0 } else { 4 + 2 * nd },
            write_buffer_flush: v & (1 << 4) != 0,
            caching_mode: v & (1 << 7) != 0,
            sagaw: ((v >> 8) & 0x1F) as u8,
            mgaw: ((v >> 16) & 0x3F) as u8 + 1,
            fault_offset: ((v >> 24) & 0x3FF) * 16,
            fault_records: ((v >> 40) & 0xFF) as u16 + 1,
            large_pages: ((v >> 34) & 0xF) as u8,
            page_invalidation: v & (1 << 39) != 0,
            max_address_mask: ((v >> 48) & 0x3F) as u8,
            drain_writes: v & (1 << 54) != 0,
            drain_reads: v & (1 << 55) != 0,
            enhanced_sirtp: v & (1 << 62) != 0,
            enhanced_srtp: v & (1 << 63) != 0,
        }
    }

    /// Whether second-level tables of `levels` (3, 4 or 5) are supported.
    pub fn supports_levels(&self, levels: u8) -> bool {
        (3..=5).contains(&levels) && self.sagaw & (1 << (levels - 2)) != 0
    }

    /// The table depth to use: 4 levels (48-bit) if supported, else 3 (39-bit). Both reach
    /// the IOVA window; 5-level tables are never needed for it.
    pub fn table_levels(&self) -> Option<u8> {
        [4, 3].into_iter().find(|&l| self.supports_levels(l))
    }
}

/// The extended capability register (chapter 10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtCapability {
    /// C: hardware walks of the remapping structures snoop the CPU caches. Clear: software
    /// must flush every structure line it writes.
    pub coherent: bool,
    /// QI: queued invalidation.
    pub queued_invalidation: bool,
    /// DT: device-TLBs.
    pub device_tlb: bool,
    /// IR: interrupt remapping.
    pub interrupt_remapping: bool,
    /// EIM: extended interrupt mode (32-bit x2APIC destinations in IRTEs).
    pub extended_interrupt_mode: bool,
    /// PT: pass-through translation type.
    pub pass_through: bool,
    /// SC: snoop control.
    pub snoop_control: bool,
    /// IRO: byte offset of the IOTLB registers.
    pub iotlb_offset: u64,
    /// MHMV: the largest handle mask an interrupt entry cache invalidation takes.
    pub max_handle_mask: u8,
    /// SMTS: scalable-mode translation.
    pub scalable_mode: bool,
}

impl ExtCapability {
    pub fn decode(v: u64) -> Self {
        Self {
            coherent: v & 1 != 0,
            queued_invalidation: v & (1 << 1) != 0,
            device_tlb: v & (1 << 2) != 0,
            interrupt_remapping: v & (1 << 3) != 0,
            extended_interrupt_mode: v & (1 << 4) != 0,
            pass_through: v & (1 << 6) != 0,
            snoop_control: v & (1 << 7) != 0,
            iotlb_offset: ((v >> 8) & 0x3FF) * 16,
            max_handle_mask: ((v >> 20) & 0xF) as u8,
            scalable_mode: v & (1 << 43) != 0,
        }
    }
}

/// A 4 KiB page: every table, and the unit of mapping.
pub const PAGE: u64 = 4096;
/// Entries in a root, context or second-level table page.
pub const ENTRIES: usize = 512;
/// Root and context entries per 4 KiB table (16 bytes each).
pub const ROOT_ENTRIES: usize = 256;
/// The address bits of a table pointer or a page (51:12).
const ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;

/// A present legacy-mode root entry pointing at the context table `context_table` (chapter
/// 9): low qword `CTP | P`, high qword zero. `None` if the table is not 4 KiB aligned or
/// lies above 52 bits.
pub fn root_entry(context_table: u64) -> Option<[u64; 2]> {
    (context_table & !ADDRESS == 0 && context_table != 0).then_some([context_table | 1, 0])
}

/// A legacy-mode context entry (chapter 9) for one requester: its domain, and the root of
/// its second-level tables. Translation type 0: untranslated requests are translated by the
/// second-level tables; device-TLBs are not used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextEntry {
    pub domain: u16,
    pub page_table: u64,
    /// 3, 4 or 5 levels (the AW field is levels − 2).
    pub levels: u8,
    /// FPD: faults from this requester are not recorded.
    pub fault_processing_disable: bool,
}

impl ContextEntry {
    /// The 128-bit entry: low `SLPTPTR | TT 0 | FPD | P`, high `DID << 8 | AW`.
    pub fn encode(&self) -> Option<[u64; 2]> {
        if self.page_table & !ADDRESS != 0 || self.page_table == 0 {
            return None;
        }
        if !(3..=5).contains(&self.levels) {
            return None;
        }
        let lo = self.page_table | u64::from(self.fault_processing_disable) << 1 | 1;
        let hi = u64::from(self.domain) << 8 | u64::from(self.levels - 2);
        Some([lo, hi])
    }

    /// The entry back, or `None` if it is not present, not translation type 0, or has an AW
    /// outside 3–5 levels.
    pub fn decode(e: [u64; 2]) -> Option<Self> {
        let [lo, hi] = e;
        if lo & 1 == 0 || (lo >> 2) & 3 != 0 {
            return None;
        }
        let levels = (hi & 7) as u8 + 2;
        (3..=5).contains(&levels).then_some(Self {
            domain: (hi >> 8) as u16,
            page_table: lo & ADDRESS,
            levels,
            fault_processing_disable: lo & 2 != 0,
        })
    }
}

/// Second-level entry bits.
pub mod sl {
    /// R: reads allowed through this entry.
    pub const READ: u64 = 1 << 0;
    /// W: writes allowed through this entry.
    pub const WRITE: u64 = 1 << 1;
}

/// What a mapping allows the device to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    pub read: bool,
    pub write: bool,
}

impl Access {
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
    };
    pub const READ: Self = Self {
        read: true,
        write: false,
    };
    pub const WRITE: Self = Self {
        read: false,
        write: true,
    };

    fn bits(self) -> u64 {
        let read = if self.read { sl::READ } else { 0 };
        let write = if self.write { sl::WRITE } else { 0 };
        read | write
    }
}

/// The memory second-level tables live in, as the kernel or a test sees it.
pub trait TableMemory {
    /// Entry `index` (below [`ENTRIES`]) of the table page at physical `table`.
    fn read(&mut self, table: u64, index: usize) -> u64;
    /// Writes entry `index` of the table page at `table`. Once the walker has written it, the
    /// entry must be visible to the hardware before the hardware can need it.
    fn write(&mut self, table: u64, index: usize, value: u64);
    /// A new zeroed table page: its physical address (4 KiB aligned).
    fn new_table(&mut self) -> Option<u64>;
}

/// Why a mapping change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    /// The IOVA or the physical address is not 4 KiB aligned, or the address lies above 52
    /// bits.
    Misaligned,
    /// The IOVA is beyond what `levels` tables translate.
    OutOfRange,
    /// The tables are not 3, 4 or 5 levels deep.
    Levels,
    /// The IOVA is already mapped (a live mapping is never replaced silently).
    AlreadyMapped,
    /// The IOVA is not mapped.
    NotMapped,
    /// No memory for a table page.
    NoMemory,
    /// A mapping must allow reading or writing.
    NoAccess,
}

/// The table index of `iova` at `level` (1 is the leaf level).
fn index(iova: u64, level: u8) -> usize {
    ((iova >> (12 + 9 * u32::from(level - 1))) & 0x1FF) as usize
}

/// Whether a second-level entry is present (hardware treats R = W = 0 as not present).
fn present(e: u64) -> bool {
    e & (sl::READ | sl::WRITE) != 0
}

/// Checks the depth and the IOVA's range.
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

/// Maps the 4 KiB page at `iova` to physical `phys` with `access`, in the `levels`-deep
/// tables rooted at `root`, creating table pages as needed. Non-leaf entries allow reading
/// and writing, so the leaf alone decides. An IOVA that is already mapped is refused.
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
        table = if present(e) {
            if e & (1 << 7) != 0 {
                // A large page already covers this IOVA.
                return Err(MapError::AlreadyMapped);
            }
            e & ADDRESS
        } else {
            let next = mem.new_table().ok_or(MapError::NoMemory)?;
            if next & !ADDRESS != 0 {
                return Err(MapError::Misaligned);
            }
            mem.write(table, i, next | sl::READ | sl::WRITE);
            next
        };
    }
    let i = index(iova, 1);
    if present(mem.read(table, i)) {
        return Err(MapError::AlreadyMapped);
    }
    mem.write(table, i, phys | access.bits());
    Ok(())
}

/// Removes the mapping of `iova`; returns the physical page it mapped. The caller must then
/// invalidate the IOTLB before it reuses the page.
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
        if !present(e) || e & (1 << 7) != 0 {
            return Err(MapError::NotMapped);
        }
        table = e & ADDRESS;
    }
    let i = index(iova, 1);
    let leaf = mem.read(table, i);
    if !present(leaf) {
        return Err(MapError::NotMapped);
    }
    mem.write(table, i, 0);
    Ok(leaf & ADDRESS)
}

/// How the tables translate `iova`, as the hardware would: the physical page and the access
/// every level allows; `None` if a level is not present.
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
        if !present(e) {
            return None;
        }
        read &= e & sl::READ != 0;
        write &= e & sl::WRITE != 0;
        if level == 1 {
            return Some((e & ADDRESS, Access { read, write }));
        }
        if e & (1 << 7) != 0 {
            return None; // a large page: never made by `map`
        }
        table = e & ADDRESS;
    }
    None
}

/// How an IRTE checks who sent an interrupt (the SVT and SQ fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceCheck {
    /// SVT 0: no check.
    None,
    /// SVT 1: the requester ID must equal `source_id`, ignoring the function bits that
    /// `qualifier` (SQ, 0–3) masks.
    RequesterId { source_id: u16, qualifier: u8 },
    /// SVT 2: the requester's bus must lie in `start..=end`.
    BusRange { start: u8, end: u8 },
}

/// An interrupt remapping table entry for a remapped (not posted) interrupt (chapter 9):
/// fixed delivery, physical destination mode, edge-triggered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Irte {
    pub vector: u8,
    /// The destination APIC ID: a 32-bit x2APIC ID in extended interrupt mode, else an
    /// 8-bit xAPIC ID.
    pub destination: u32,
    pub source: SourceCheck,
    /// FPD: faults for this entry are not recorded.
    pub fault_processing_disable: bool,
}

/// Bits of an IRTE's low qword: present, FPD, the vector and the destination field.
const IRTE_PRESENT: u64 = 1;
const IRTE_FPD: u64 = 1 << 1;

impl Irte {
    /// The 128-bit entry. Low qword: P, FPD, DM 0 (physical), RH 0, TM 0 (edge), DLM 000
    /// (fixed), IM 0 (remapped), the vector in bits 23:16 and the destination in 63:32 (in
    /// x2APIC mode the whole ID; in xAPIC mode the 8-bit ID in bits 47:40). High qword: SID,
    /// SQ and SVT. `x2apic` is the table's EIME. `None` for a vector below 16, an xAPIC
    /// destination above 255, or a qualifier above 3.
    pub fn encode(&self, x2apic: bool) -> Option<[u64; 2]> {
        if self.vector < 16 {
            return None;
        }
        let destination = if x2apic {
            u64::from(self.destination) << 32
        } else {
            u64::from(u8::try_from(self.destination).ok()?) << 40
        };
        let fpd = if self.fault_processing_disable {
            IRTE_FPD
        } else {
            0
        };
        let lo = destination | u64::from(self.vector) << 16 | fpd | IRTE_PRESENT;
        let hi = match self.source {
            SourceCheck::None => 0,
            SourceCheck::RequesterId {
                source_id,
                qualifier,
            } => {
                if qualifier > 3 {
                    return None;
                }
                1 << 18 | u64::from(qualifier) << 16 | u64::from(source_id)
            }
            SourceCheck::BusRange { start, end } => {
                2 << 18 | u64::from(start) << 8 | u64::from(end)
            }
        };
        Some([lo, hi])
    }

    /// The entry back, or `None` if it is not present, is posted, or uses a delivery mode,
    /// trigger mode or destination mode `encode` never writes.
    pub fn decode(e: [u64; 2], x2apic: bool) -> Option<Self> {
        let [lo, hi] = e;
        if lo & IRTE_PRESENT == 0 || lo & 0b1111_1100 != 0 || lo & (1 << 15) != 0 {
            return None;
        }
        let destination = if x2apic {
            (lo >> 32) as u32
        } else {
            ((lo >> 40) & 0xFF) as u32
        };
        let sid = (hi & 0xFFFF) as u16;
        let source = match (hi >> 18) & 3 {
            0 => SourceCheck::None,
            1 => SourceCheck::RequesterId {
                source_id: sid,
                qualifier: ((hi >> 16) & 3) as u8,
            },
            2 => SourceCheck::BusRange {
                start: (sid >> 8) as u8,
                end: sid as u8,
            },
            _ => return None,
        };
        Some(Self {
            vector: (lo >> 16) as u8,
            destination,
            source,
            fault_processing_disable: lo & IRTE_FPD != 0,
        })
    }
}

/// An MSI or MSI-X message: the address the device writes and the data it writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsiMessage {
    pub address: u64,
    pub data: u32,
}

/// The interrupt address range's fixed head, `FEEh` in address bits 31:20.
const INTERRUPT_HEAD: u64 = 0xFEE0_0000;
/// Address bit 4: the message is in remappable format.
const REMAPPABLE: u64 = 1 << 4;
/// Address bit 3: SHV, the data carries a sub-handle.
const SUBHANDLE_VALID: u64 = 1 << 3;

/// The remappable-format message that selects IRTE `index` (chapter 5): the handle is the
/// index (address bits 19:5 hold its low 15 bits, bit 2 its bit 15), SHV is set and the
/// sub-handle in the data is 0. The vector and destination exist only in the IRTE.
pub fn remappable_msi(index: u16) -> MsiMessage {
    let handle = u64::from(index);
    MsiMessage {
        address: INTERRUPT_HEAD
            | (handle & 0x7FFF) << 5
            | REMAPPABLE
            | SUBHANDLE_VALID
            | (handle >> 15) << 2,
        data: 0,
    }
}

/// The IRTE index the hardware takes from `m`: the handle, plus the data's sub-handle when
/// SHV is set. `None` if `m` is not a remappable-format interrupt message (above 4 GiB, not
/// in the `FEEx_xxxx` range, the format bit clear) or, with SHV, has data bits 31:16 set.
pub fn interrupt_index(m: MsiMessage) -> Option<u16> {
    if m.address >> 32 != 0 || m.address & 0xFFF0_0000 != INTERRUPT_HEAD {
        return None;
    }
    if m.address & REMAPPABLE == 0 {
        return None;
    }
    let handle = ((m.address >> 5) & 0x7FFF) | ((m.address >> 2) & 1) << 15;
    if m.address & SUBHANDLE_VALID == 0 {
        return u16::try_from(handle).ok();
    }
    if m.data >> 16 != 0 {
        return None;
    }
    u16::try_from(handle + u64::from(m.data & 0xFFFF)).ok()
}

/// The IRTA register (chapter 10): the table's base, its size as 2^(S+1) entries, and EIME
/// (extended interrupt mode: 32-bit x2APIC destinations). `entries` must be a power of two
/// from 2 to 65536 and the table 4 KiB aligned.
pub fn irta(table: u64, entries: u32, x2apic: bool) -> Option<u64> {
    if table & !ADDRESS != 0 || table == 0 || !entries.is_power_of_two() {
        return None;
    }
    let s = entries.trailing_zeros().checked_sub(1)?;
    (s <= 15).then_some(table | u64::from(x2apic) << 11 | u64::from(s))
}

/// The IQA register (chapter 10) for a queue of 2^`order` pages (order 0–7) at `queue`
/// (aligned to its size), with 128-bit descriptors (DW 0).
pub fn iqa(queue: u64, order: u8) -> Option<u64> {
    if order > 7 || queue == 0 || queue & !ADDRESS != 0 {
        return None;
    }
    queue
        .is_multiple_of(PAGE << order)
        .then_some(queue | u64::from(order))
}

/// Descriptors in a queue of 2^`order` pages (16 bytes each).
pub fn queue_entries(order: u8) -> usize {
    256 << order.min(7)
}

/// The IQT or IQH value for descriptor `index` (128-bit descriptors: the byte offset).
pub fn queue_offset(index: usize) -> u64 {
    (index as u64) << 4
}

/// Which outstanding reads and writes an IOTLB invalidation drains (when the capability
/// register offers it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drain {
    pub reads: bool,
    pub writes: bool,
}

impl Drain {
    fn bits(self) -> u64 {
        u64::from(self.writes) << 6 | u64::from(self.reads) << 7
    }
}

/// A queued-invalidation descriptor (chapter 6), 128 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Descriptor {
    /// Context-cache invalidation of every entry.
    ContextGlobal,
    /// Of one domain's entries.
    ContextDomain { domain: u16 },
    /// Of one requester's entry; `function_mask` (FM, 0–3) ignores that many low function
    /// bits of the source ID.
    ContextDevice {
        domain: u16,
        source_id: u16,
        function_mask: u8,
    },
    /// IOTLB invalidation of every entry.
    IotlbGlobal { drain: Drain },
    /// Of one domain's entries.
    IotlbDomain { domain: u16, drain: Drain },
    /// Of 2^`order` pages from `address` (aligned to that size) in `domain`. `leaf_only`
    /// (IH) says only leaf entries changed, so cached non-leaf entries may stay.
    IotlbPages {
        domain: u16,
        address: u64,
        order: u8,
        leaf_only: bool,
        drain: Drain,
    },
    /// Interrupt entry cache invalidation of every entry.
    IecGlobal,
    /// Of 2^`order` entries from `index` (aligned to that count).
    IecIndex { index: u16, order: u8 },
    /// Writes `status` to `address` (4-byte aligned) once every earlier descriptor has
    /// completed; with `fence`, later descriptors wait for it.
    Wait {
        address: u64,
        status: u32,
        fence: bool,
    },
}

impl Descriptor {
    /// The two qwords, or `None` for a field that does not fit (a misaligned address, an
    /// order or mask too large).
    pub fn encode(&self) -> Option<[u64; 2]> {
        Some(match *self {
            Self::ContextGlobal => [0x1 | 1 << 4, 0],
            Self::ContextDomain { domain } => [0x1 | 2 << 4 | u64::from(domain) << 16, 0],
            Self::ContextDevice {
                domain,
                source_id,
                function_mask,
            } => {
                if function_mask > 3 {
                    return None;
                }
                [
                    0x1 | 3 << 4
                        | u64::from(domain) << 16
                        | u64::from(source_id) << 32
                        | u64::from(function_mask) << 48,
                    0,
                ]
            }
            Self::IotlbGlobal { drain } => [0x2 | 1 << 4 | drain.bits(), 0],
            Self::IotlbDomain { domain, drain } => {
                [0x2 | 2 << 4 | drain.bits() | u64::from(domain) << 16, 0]
            }
            Self::IotlbPages {
                domain,
                address,
                order,
                leaf_only,
                drain,
            } => {
                if order > 51 || address & !ADDRESS != 0 {
                    return None;
                }
                let span = PAGE.checked_shl(u32::from(order))?;
                if !address.is_multiple_of(span) {
                    return None;
                }
                [
                    0x2 | 3 << 4 | drain.bits() | u64::from(domain) << 16,
                    address | u64::from(leaf_only) << 6 | u64::from(order),
                ]
            }
            Self::IecGlobal => [0x4, 0],
            Self::IecIndex { index, order } => {
                if order > 31 || u32::from(index) % (1u32 << order) != 0 {
                    return None;
                }
                [
                    0x4 | 1 << 4 | u64::from(order) << 27 | u64::from(index) << 32,
                    0,
                ]
            }
            Self::Wait {
                address,
                status,
                fence,
            } => {
                if address & 3 != 0 || address == 0 {
                    return None;
                }
                [
                    0x5 | 1 << 5 | u64::from(fence) << 6 | u64::from(status) << 32,
                    address,
                ]
            }
        })
    }
}

/// Fault status register bits (chapter 10).
pub mod fsts {
    /// PFO: a fault was lost because every recording register was full.
    pub const PFO: u32 = 1 << 0;
    /// PPF: a fault-recording register holds a fault.
    pub const PPF: u32 = 1 << 1;
    /// IQE: an invalid invalidation descriptor stopped the queue.
    pub const IQE: u32 = 1 << 4;
    /// ICE: an invalidation completion error (device-TLB).
    pub const ICE: u32 = 1 << 5;
    /// ITE: an invalidation time-out (device-TLB).
    pub const ITE: u32 = 1 << 6;
    /// The write-1-to-clear bits.
    pub const CLEAR: u32 = PFO | IQE | ICE | ITE;
}

/// The F bit of a fault-recording register's high qword (write 1 to clear).
pub const FRCD_FAULT: u64 = 1 << 63;

/// One recorded non-recoverable fault (chapter 7, the fault-recording register).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultRecord {
    /// The requester ID of the faulting request.
    pub source_id: u16,
    /// The fault reason ([`fault_reason`]).
    pub reason: u8,
    /// T: the request was a read; otherwise a write.
    pub read: bool,
    /// FI: the faulting page, for a DMA fault.
    pub address: u64,
    /// For an interrupt-remapping fault (reasons 20h–2Fh): the interrupt index (FI 63:48).
    pub interrupt_index: Option<u16>,
}

/// Decodes a fault-recording register (low and high qwords); `None` when its F bit is clear.
pub fn fault_record(lo: u64, hi: u64) -> Option<FaultRecord> {
    if hi & FRCD_FAULT == 0 {
        return None;
    }
    let reason = ((hi >> 32) & 0xFF) as u8;
    let interrupt = (0x20..=0x2F).contains(&reason);
    Some(FaultRecord {
        source_id: (hi & 0xFFFF) as u16,
        reason,
        read: hi & (1 << 62) != 0,
        address: if interrupt { 0 } else { lo & !(PAGE - 1) },
        interrupt_index: interrupt.then_some((lo >> 48) as u16),
    })
}

/// What a non-recoverable fault reason means (the specification's appendix of fault
/// reasons), for the reasons this driver can provoke or meet.
pub fn fault_reason(reason: u8) -> &'static str {
    match reason {
        0x01 => "root entry not present",
        0x02 => "context entry not present",
        0x03 => "context entry invalid",
        0x04 => "address beyond the domain's width",
        0x05 => "write to a page not mapped writable",
        0x06 => "read from a page not mapped readable",
        0x07 => "error reading a paging entry",
        0x08 => "error reading the root table",
        0x09 => "error reading a context table",
        0x0A => "reserved bits set in a root entry",
        0x0B => "reserved bits set in a context entry",
        0x0C => "reserved bits set in a paging entry",
        0x0D => "request blocked by the translation type",
        0x20 => "interrupt request with reserved bits set",
        0x21 => "interrupt index beyond the table",
        0x22 => "interrupt table entry not present",
        0x23 => "error reading the interrupt table",
        0x24 => "reserved bits set in an interrupt table entry",
        0x25 => "compatibility-format interrupt blocked",
        0x26 => "interrupt source-ID check failed",
        _ => "other",
    }
}

/// The IOVA window every domain allocates from (ADR-0007 D4): [1 GiB, 2 GiB). Below every PC
/// platform's 32-bit MMIO hole, clear of the interrupt range, reachable by 32-bit DMA.
pub const IOVA_BASE: u64 = 1 << 30;
pub const IOVA_END: u64 = 2 << 30;

/// A bump allocator of IOVA pages over a window. Addresses are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IovaWindow {
    next: u64,
    end: u64,
}

impl IovaWindow {
    /// The window `[base, end)`, both 4 KiB aligned.
    pub const fn new(base: u64, end: u64) -> Self {
        Self { next: base, end }
    }

    /// `pages` contiguous IOVA pages, or `None` when the window is exhausted.
    pub fn alloc(&mut self, pages: u64) -> Option<u64> {
        let bytes = pages.checked_mul(PAGE)?;
        let at = self.next;
        let next = at.checked_add(bytes)?;
        if pages == 0 || next > self.end {
            return None;
        }
        self.next = next;
        Some(at)
    }

    /// Whether `iova` was handed out by this window.
    pub fn issued(&self, iova: u64, base: u64) -> bool {
        (base..self.next).contains(&iova)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Table pages in a map, as the hardware would see them, with a page budget.
    #[derive(Default)]
    struct Memory {
        tables: BTreeMap<u64, [u64; ENTRIES]>,
        next: u64,
        budget: usize,
    }

    impl Memory {
        fn new(budget: usize) -> (Self, u64) {
            let mut m = Self {
                next: 0x10_0000,
                budget: budget + 1,
                ..Self::default()
            };
            let root = m.new_table().unwrap();
            (m, root)
        }
    }

    impl TableMemory for Memory {
        fn read(&mut self, table: u64, index: usize) -> u64 {
            self.tables.get(&table).map_or(0, |t| t[index])
        }
        fn write(&mut self, table: u64, index: usize, value: u64) {
            self.tables
                .get_mut(&table)
                .expect("a table the walker made")[index] = value;
        }
        fn new_table(&mut self) -> Option<u64> {
            self.budget = self.budget.checked_sub(1)?;
            let at = self.next;
            self.next += PAGE;
            self.tables.insert(at, [0; ENTRIES]);
            Some(at)
        }
    }

    /// The capability values QEMU 11.1 builds for `intel-iommu,intremap=on,eim=on` with the
    /// default 48-bit address width (hw/i386/intel_iommu.c, `vtd_cap_init`).
    const QEMU_CAP: u64 = 6 // ND: 16-bit domain IDs
        | 0b0110 << 8 // SAGAW: 39-bit and 48-bit
        | 47 << 16 // MGAW
        | 0x22 << 24 // FRO: 0x220
        | 0b11 << 34 // SLLPS
        | 1 << 39 // PSI
        | 18 << 48 // MAMV
        | 1 << 54 | 1 << 55 // DWD, DRD
        | 1 << 63; // ESRTPS
    const QEMU_ECAP: u64 = 1 << 1 | 1 << 3 | 1 << 4 | 1 << 6 | 0xF << 8 | 0xF << 20;

    #[test]
    fn capabilities_decode() {
        let c = Capability::decode(QEMU_CAP);
        assert_eq!(c.domain_bits, 16);
        assert_eq!((c.sagaw, c.mgaw), (0b0110, 48));
        assert_eq!((c.fault_offset, c.fault_records), (0x220, 1));
        assert!(c.page_invalidation && c.drain_reads && c.drain_writes && c.enhanced_srtp);
        assert!(!c.caching_mode && !c.write_buffer_flush && !c.enhanced_sirtp);
        assert_eq!((c.max_address_mask, c.large_pages), (18, 0b11));
        assert!(c.supports_levels(3) && c.supports_levels(4) && !c.supports_levels(5));
        assert!(!c.supports_levels(2) && !c.supports_levels(6));
        assert_eq!(c.table_levels(), Some(4));
        assert_eq!(Capability::decode(0b0010 << 8).table_levels(), Some(3));
        assert_eq!(Capability::decode(0b1000 << 8).table_levels(), None);
        assert_eq!(Capability::decode(7).domain_bits, 0, "ND 7 is reserved");
        let e = ExtCapability::decode(QEMU_ECAP);
        assert!(e.queued_invalidation && e.interrupt_remapping && e.extended_interrupt_mode);
        assert!(e.pass_through && !e.coherent && !e.device_tlb && !e.snoop_control);
        assert_eq!((e.iotlb_offset, e.max_handle_mask), (0xF0, 15));
        assert!(!e.scalable_mode);
        assert_eq!(version(0x10), (1, 0));
    }

    #[test]
    fn global_command_keeps_enables_and_drops_one_shot_bits() {
        // Translation and QI are on, and the root-pointer status is still set.
        let gsts = gcmd::TE | gcmd::QIE | gcmd::SRTP | gcmd::SIRTP | gcmd::WBF | gcmd::SFL;
        assert_eq!(gcmd(gsts, gcmd::IRE, 0), gcmd::TE | gcmd::QIE | gcmd::IRE);
        assert_eq!(gcmd(gsts, gcmd::SRTP, 0), gcmd::TE | gcmd::QIE | gcmd::SRTP);
        assert_eq!(gcmd(gsts, 0, gcmd::TE), gcmd::QIE);
        assert_eq!(gcmd(0, gcmd::TE, 0), gcmd::TE);
        assert_eq!(gcmd(gcmd::CFI | gcmd::IRE, 0, gcmd::CFI), gcmd::IRE);
    }

    #[test]
    fn root_and_context_entries() {
        assert_eq!(root_entry(0x1234_5000), Some([0x1234_5001, 0]));
        assert_eq!(root_entry(0x1234_5800), None);
        assert_eq!(root_entry(0), None);
        assert_eq!(root_entry(1 << 52), None);
        let c = ContextEntry {
            domain: 0x1234,
            page_table: 0xABC_D000,
            levels: 4,
            fault_processing_disable: false,
        };
        let e = c.encode().unwrap();
        assert_eq!(e, [0xABC_D001, 0x12_3402]);
        assert_eq!(ContextEntry::decode(e), Some(c));
        let fpd = ContextEntry {
            fault_processing_disable: true,
            levels: 3,
            ..c
        };
        assert_eq!(fpd.encode().unwrap(), [0xABC_D003, 0x12_3401]);
        assert_eq!(ContextEntry::decode(fpd.encode().unwrap()), Some(fpd));
        for bad in [
            ContextEntry { levels: 2, ..c },
            ContextEntry { levels: 6, ..c },
            ContextEntry {
                page_table: 0xABC_D008,
                ..c
            },
            ContextEntry { page_table: 0, ..c },
        ] {
            assert_eq!(bad.encode(), None, "{bad:?}");
        }
        assert_eq!(ContextEntry::decode([0xABC_D000, 2]), None, "not present");
        assert_eq!(ContextEntry::decode([0xABC_D009, 2]), None, "pass-through");
        assert_eq!(ContextEntry::decode([0xABC_D001, 7]), None, "AW 7");
    }

    #[test]
    fn second_level_tables_map_translate_and_unmap() {
        let (mut m, root) = Memory::new(64);
        let a = IOVA_BASE;
        map(&mut m, root, 4, a, 0x7_6543_2000, Access::READ_WRITE).unwrap();
        // One table per level below the root.
        assert_eq!(m.tables.len(), 4);
        assert_eq!(
            translate(&mut m, root, 4, a),
            Some((0x7_6543_2000, Access::READ_WRITE))
        );
        assert_eq!(
            translate(&mut m, root, 4, a + 0x123),
            Some((0x7_6543_2000, Access::READ_WRITE)),
            "an offset inside the page"
        );
        // The neighbour shares every table; a page 1 GiB away needs new level-2 and
        // level-1 tables (the level-3 table is shared: both are under one PML4 slot).
        map(&mut m, root, 4, a + PAGE, 0x2000, Access::READ).unwrap();
        assert_eq!(m.tables.len(), 4);
        map(&mut m, root, 4, a + (1 << 30), 0x3000, Access::WRITE).unwrap();
        assert_eq!(m.tables.len(), 6);
        assert_eq!(
            translate(&mut m, root, 4, a + PAGE),
            Some((0x2000, Access::READ))
        );
        assert_eq!(
            translate(&mut m, root, 4, a + (1 << 30)),
            Some((0x3000, Access::WRITE))
        );
        assert_eq!(translate(&mut m, root, 4, a + 2 * PAGE), None);
        // Non-leaf entries allow both, so the leaf alone decides.
        for t in m.tables.values() {
            for e in t.iter().filter(|e| **e != 0) {
                assert!(*e & 3 != 0);
            }
        }
        // A live mapping is never replaced; refusals.
        assert_eq!(
            map(&mut m, root, 4, a, 0x9000, Access::READ),
            Err(MapError::AlreadyMapped)
        );
        assert_eq!(
            map(&mut m, root, 4, a + 1, 0x9000, Access::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            map(&mut m, root, 4, a, 0x9001, Access::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            map(&mut m, root, 4, a + 3 * PAGE, 1 << 52, Access::READ),
            Err(MapError::Misaligned)
        );
        assert_eq!(
            map(&mut m, root, 4, 1 << 48, 0x9000, Access::READ),
            Err(MapError::OutOfRange)
        );
        assert_eq!(
            map(&mut m, root, 3, 1 << 39, 0x9000, Access::READ),
            Err(MapError::OutOfRange)
        );
        assert_eq!(
            map(&mut m, root, 2, a, 0x9000, Access::READ),
            Err(MapError::Levels)
        );
        let none = Access {
            read: false,
            write: false,
        };
        assert_eq!(
            map(&mut m, root, 4, a + 3 * PAGE, 0x9000, none),
            Err(MapError::NoAccess)
        );
        // Unmap returns the page, and the IOVA no longer translates.
        assert_eq!(unmap(&mut m, root, 4, a), Ok(0x7_6543_2000));
        assert_eq!(translate(&mut m, root, 4, a), None);
        assert_eq!(unmap(&mut m, root, 4, a), Err(MapError::NotMapped));
        assert_eq!(
            unmap(&mut m, root, 4, a + (1 << 35)),
            Err(MapError::NotMapped)
        );
        // It can be mapped again afterwards.
        map(&mut m, root, 4, a, 0x5000, Access::READ_WRITE).unwrap();
        assert_eq!(
            translate(&mut m, root, 4, a),
            Some((0x5000, Access::READ_WRITE))
        );
    }

    #[test]
    fn running_out_of_table_pages_is_reported() {
        let (mut m, root) = Memory::new(2);
        assert_eq!(
            map(&mut m, root, 4, IOVA_BASE, 0x1000, Access::READ),
            Err(MapError::NoMemory)
        );
        assert_eq!(translate(&mut m, root, 4, IOVA_BASE), None);
        // Three-level tables need one page less.
        let (mut m, root) = Memory::new(2);
        assert_eq!(
            map(&mut m, root, 3, IOVA_BASE, 0x1000, Access::READ),
            Ok(())
        );
    }

    /// A model check: random maps and unmaps against a plain map of what should be there.
    #[test]
    fn walker_agrees_with_a_model() {
        let (mut m, root) = Memory::new(4096);
        let mut model = BTreeMap::new();
        let mut rng = crate::testing::Rng::new(0x5E7D);
        for step in 0..crate::testing::cases(20_000) {
            // Spread IOVAs over several tables at every level.
            let iova = IOVA_BASE + (rng.below(1 << 12) as u64) * PAGE * 97 % (1 << 30);
            let iova = iova & !(PAGE - 1);
            if rng.below(3) == 0 {
                let got = unmap(&mut m, root, 4, iova);
                match model.remove(&iova) {
                    Some(phys) => assert_eq!(got, Ok(phys), "step {step}"),
                    None => assert_eq!(got, Err(MapError::NotMapped), "step {step}"),
                }
            } else {
                let phys = (rng.below(1 << 20) as u64 + 1) * PAGE;
                let got = map(&mut m, root, 4, iova, phys, Access::READ_WRITE);
                if let std::collections::btree_map::Entry::Vacant(v) = model.entry(iova) {
                    assert_eq!(got, Ok(()), "step {step}");
                    v.insert(phys);
                } else {
                    assert_eq!(got, Err(MapError::AlreadyMapped), "step {step}");
                }
            }
            if step % 997 == 0 {
                for (&iova, &phys) in &model {
                    assert_eq!(
                        translate(&mut m, root, 4, iova),
                        Some((phys, Access::READ_WRITE))
                    );
                }
            }
        }
    }

    #[test]
    fn interrupt_table_entries() {
        let nvme = Irte {
            vector: 0x40,
            destination: 3,
            source: SourceCheck::RequesterId {
                source_id: 0x0010,
                qualifier: 0,
            },
            fault_processing_disable: false,
        };
        let e = nvme.encode(true).unwrap();
        assert_eq!(e, [3 << 32 | 0x40 << 16 | 1, 1 << 18 | 0x0010]);
        assert_eq!(Irte::decode(e, true), Some(nvme));
        // x2APIC destinations above 255 need EIME; xAPIC ones sit in bits 47:40.
        for id in [0, 255, 256, 0x1_0000, 0xFFFF_FFFE] {
            let i = Irte {
                destination: id,
                ..nvme
            };
            let e = i.encode(true).unwrap();
            assert_eq!(e[0] >> 32, u64::from(id));
            assert_eq!(Irte::decode(e, true), Some(i));
        }
        let x = Irte {
            destination: 0xAB,
            ..nvme
        };
        assert_eq!(x.encode(false).unwrap()[0] >> 32, 0xAB00);
        assert_eq!(Irte::decode(x.encode(false).unwrap(), false), Some(x));
        assert_eq!(
            Irte {
                destination: 256,
                ..nvme
            }
            .encode(false),
            None
        );
        assert_eq!(Irte { vector: 15, ..nvme }.encode(true), None);
        let bus = Irte {
            source: SourceCheck::BusRange { start: 2, end: 5 },
            fault_processing_disable: true,
            ..nvme
        };
        let e = bus.encode(true).unwrap();
        assert_eq!(e[1], 2 << 18 | 0x0205);
        assert_eq!(e[0] & 2, 2);
        assert_eq!(Irte::decode(e, true), Some(bus));
        assert_eq!(
            Irte {
                source: SourceCheck::RequesterId {
                    source_id: 1,
                    qualifier: 4
                },
                ..nvme
            }
            .encode(true),
            None
        );
        assert_eq!(Irte::decode([0, 0], true), None, "not present");
        assert_eq!(Irte::decode([1 | 1 << 15, 0], true), None, "posted");
        assert_eq!(Irte::decode([1 | 1 << 4, 0], true), None, "level-triggered");
        assert_eq!(Irte::decode([1, 3 << 18], true), None, "SVT 3");
    }

    #[test]
    fn remappable_messages_carry_only_a_handle() {
        for index in [0u16, 1, 2, 0x7FFF, 0x8000, 0xFFFF, 0x1234] {
            let m = remappable_msi(index);
            assert_eq!(m.data, 0);
            assert_eq!(m.address & 0xFFF0_0000, 0xFEE0_0000);
            assert_eq!(m.address & (1 << 4), 1 << 4, "remappable format");
            assert_eq!(interrupt_index(m), Some(index));
        }
        assert_eq!(remappable_msi(1).address, 0xFEE0_0038);
        assert_eq!(remappable_msi(0x8000).address, 0xFEE0_001C);
        // A compatibility-format message (APIC ID 1, vector 0x40) selects no entry.
        let compat = MsiMessage {
            address: 0xFEE0_1000,
            data: 0x40,
        };
        assert_eq!(interrupt_index(compat), None);
        // With SHV the sub-handle is added, and data bits 31:16 must be clear.
        let mut m = remappable_msi(4);
        m.data = 3;
        assert_eq!(interrupt_index(m), Some(7));
        m.data = 1 << 16;
        assert_eq!(interrupt_index(m), None);
        // Without SHV the data is ignored.
        m.address &= !(1 << 3);
        assert_eq!(interrupt_index(m), Some(4));
        assert_eq!(
            interrupt_index(MsiMessage {
                address: 0x1_FEE0_0010,
                data: 0
            }),
            None
        );
        assert_eq!(
            interrupt_index(MsiMessage {
                address: 0xFED0_0010,
                data: 0
            }),
            None
        );
    }

    #[test]
    fn table_and_queue_registers() {
        assert_eq!(irta(0x10_0000, 256, true), Some(0x10_0000 | 1 << 11 | 7));
        assert_eq!(irta(0x10_0000, 2, false), Some(0x10_0000));
        assert_eq!(irta(0x10_0000, 65536, true), Some(0x10_0000 | 1 << 11 | 15));
        assert_eq!(irta(0x10_0000, 1, true), None);
        assert_eq!(irta(0x10_0000, 3, true), None);
        assert_eq!(irta(0x10_0000, 1 << 17, true), None);
        assert_eq!(irta(0x10_0800, 256, true), None);
        assert_eq!(iqa(0x20_0000, 0), Some(0x20_0000));
        assert_eq!(iqa(0x20_0000, 1), Some(0x20_0001));
        assert_eq!(iqa(0x20_1000, 1), None, "aligned to the queue's size");
        assert_eq!(iqa(0x20_0000, 8), None);
        assert_eq!(iqa(0, 0), None);
        assert_eq!(queue_entries(0), 256);
        assert_eq!(queue_offset(3), 0x30);
    }

    #[test]
    fn invalidation_descriptors() {
        let drain = Drain {
            reads: true,
            writes: true,
        };
        let enc = |d: Descriptor| d.encode().unwrap();
        assert_eq!(enc(Descriptor::ContextGlobal), [0x11, 0]);
        assert_eq!(enc(Descriptor::ContextDomain { domain: 5 }), [0x5_0021, 0]);
        assert_eq!(
            enc(Descriptor::ContextDevice {
                domain: 1,
                source_id: 0x0010,
                function_mask: 0
            }),
            [0x0010_0001_0031, 0]
        );
        assert_eq!(enc(Descriptor::IotlbGlobal { drain }), [0x12 | 0xC0, 0]);
        assert_eq!(
            enc(Descriptor::IotlbDomain {
                domain: 2,
                drain: Drain::default()
            }),
            [0x2_0022, 0]
        );
        assert_eq!(
            enc(Descriptor::IotlbPages {
                domain: 1,
                address: 0x4000_0000,
                order: 0,
                leaf_only: false,
                drain
            }),
            [0x1_00F2, 0x4000_0000]
        );
        assert_eq!(
            enc(Descriptor::IotlbPages {
                domain: 1,
                address: 0x4000_8000,
                order: 3,
                leaf_only: true,
                drain: Drain::default()
            }),
            [0x1_0032, 0x4000_8000 | 1 << 6 | 3]
        );
        assert_eq!(
            Descriptor::IotlbPages {
                domain: 1,
                address: 0x4000_1000,
                order: 1,
                leaf_only: false,
                drain
            }
            .encode(),
            None,
            "not aligned to two pages"
        );
        assert_eq!(enc(Descriptor::IecGlobal), [0x4, 0]);
        assert_eq!(
            enc(Descriptor::IecIndex { index: 8, order: 2 }),
            [0x4 | 1 << 4 | 2 << 27 | 8 << 32, 0]
        );
        assert_eq!(Descriptor::IecIndex { index: 9, order: 2 }.encode(), None);
        assert_eq!(
            enc(Descriptor::Wait {
                address: 0x9_0004,
                status: 0xC0DE,
                fence: true
            }),
            [0x5 | 1 << 5 | 1 << 6 | 0xC0DE << 32, 0x9_0004]
        );
        assert_eq!(
            Descriptor::Wait {
                address: 0x9_0002,
                status: 1,
                fence: false
            }
            .encode(),
            None
        );
        assert_eq!(
            Descriptor::ContextDevice {
                domain: 1,
                source_id: 1,
                function_mask: 4
            }
            .encode(),
            None
        );
    }

    #[test]
    fn fault_records_decode() {
        assert_eq!(fault_record(0x4000_0000, 5 << 32 | 0x10), None, "F clear");
        // NVMe (00:02.0) writing an unmapped page: reason 5, T clear.
        let w = fault_record(0x4000_0123, FRCD_FAULT | 5 << 32 | 0x0010).unwrap();
        assert_eq!(
            w,
            FaultRecord {
                source_id: 0x0010,
                reason: 5,
                read: false,
                address: 0x4000_0000,
                interrupt_index: None
            }
        );
        assert_eq!(
            fault_reason(w.reason),
            "write to a page not mapped writable"
        );
        let r = fault_record(0x4000_1000, FRCD_FAULT | 1 << 62 | 2 << 32 | 0x00F8).unwrap();
        assert!(r.read && r.reason == 2 && r.source_id == 0xF8);
        // An interrupt fault carries the index in FI 63:48.
        let i = fault_record(7 << 48, FRCD_FAULT | 0x22 << 32 | 0x0010).unwrap();
        assert_eq!((i.interrupt_index, i.address), (Some(7), 0));
        assert_eq!(fault_reason(0x22), "interrupt table entry not present");
        assert_eq!(fault_reason(0xEE), "other");
        // Any register contents decode without panicking.
        let mut rng = crate::testing::Rng::new(0xFA17);
        for _ in 0..crate::testing::cases(20_000) {
            let (lo, hi) = (rng.next(), rng.next());
            if let Some(f) = fault_record(lo, hi) {
                assert!(hi & FRCD_FAULT != 0);
                let _ = fault_reason(f.reason);
            }
        }
    }

    #[test]
    fn iova_window_hands_out_pages_once() {
        let mut w = IovaWindow::new(IOVA_BASE, IOVA_BASE + 4 * PAGE);
        assert_eq!(w.alloc(1), Some(IOVA_BASE));
        assert_eq!(w.alloc(2), Some(IOVA_BASE + PAGE));
        assert!(w.issued(IOVA_BASE + 2 * PAGE, IOVA_BASE));
        assert!(!w.issued(IOVA_BASE + 3 * PAGE, IOVA_BASE));
        assert_eq!(w.alloc(2), None);
        assert_eq!(w.alloc(0), None);
        assert_eq!(w.alloc(1), Some(IOVA_BASE + 3 * PAGE));
        assert_eq!(w.alloc(1), None);
        assert_eq!(IovaWindow::new(0, u64::MAX).alloc(u64::MAX), None);
    }
}
