//! PCI Express configuration space (CLAUDE.md §32: host bridge discovery, enumeration,
//! configuration access, BAR mapping), over an abstract [`ConfigSpace`] so all of it is
//! tested on the host.
//!
//! References:
//! - PCI Firmware Specification 3.3 §4.1.2: the ACPI MCFG table, one allocation per ECAM
//!   window (base address of bus 0, segment group, start and end bus);
//! - PCI Express Base Specification 6.x §7.2.2 (ECAM: `bus << 20 | device << 15 |
//!   function << 12 | register`) and §7.5.1 (the type 0 and type 1 configuration headers:
//!   IDs at 00h, command 04h, status 06h, revision and class 08h–0Bh, header type 0Eh,
//!   BARs from 10h; §7.5.1.2.1 sizing a BAR by writing all ones).
//!
//! Configuration space is untrusted input: a device can report anything. Every value is
//! decoded with checked arithmetic, and sizing restores each BAR and the command register.

use core::fmt;
use core::ops::RangeInclusive;

/// Configuration registers this module uses.
pub mod register {
    pub const ID: u16 = 0x00;
    pub const COMMAND: u16 = 0x04;
    pub const CLASS: u16 = 0x08;
    /// The dword holding cache line size, latency timer, header type (byte 0Eh) and BIST.
    pub const HEADER: u16 = 0x0C;
    pub const BAR0: u16 = 0x10;
}

/// Status register bit 4 (in the upper half of the command dword): a capability list.
const STATUS_CAPABILITIES: u32 = 1 << 20;
/// The capability list's head pointer (type 0 and type 1 headers).
const CAPABILITIES_POINTER: u16 = 0x34;
/// Capability IDs (PCI Code and ID Assignment Specification).
pub mod capability {
    pub const MSI: u8 = 0x05;
    pub const PCI_EXPRESS: u8 = 0x10;
    pub const MSIX: u8 = 0x11;
}

/// Command register bits 0 and 1: I/O space and memory space decoding.
const DECODE: u32 = 0b11;
/// Header type bit 7: the device has several functions.
const MULTIFUNCTION: u8 = 0x80;

/// A function's location in one segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

/// `bb:dd.f`, as PCI tools print it.
impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}:{:02x}.{}", self.bus, self.device, self.function)
    }
}

/// Reads and writes 32-bit configuration registers. Register offsets are dword-aligned
/// and below 4096.
pub trait ConfigSpace {
    fn read32(&mut self, at: Address, register: u16) -> u32;
    fn write32(&mut self, at: Address, register: u16, value: u32);
}

/// One ECAM window from the MCFG.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Ecam {
    /// Physical address of bus 0's configuration space in this segment.
    pub base: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

impl Ecam {
    /// The physical range that holds the window's buses: start and length.
    pub fn window(&self) -> Option<(u64, u64)> {
        let start = self.base.checked_add(u64::from(self.start_bus) << 20)?;
        let buses = u64::from(self.end_bus.checked_sub(self.start_bus)?) + 1;
        let len = buses << 20;
        start.checked_add(len)?;
        Some((start, len))
    }

    /// The offset of `register` of the function at `at` from the window's start, or `None`
    /// outside the window or for a misaligned register.
    pub fn offset(&self, at: Address, register: u16) -> Option<u64> {
        if !(self.start_bus..=self.end_bus).contains(&at.bus)
            || at.device >= 32
            || at.function >= 8
            || register >= 4096
            || !register.is_multiple_of(4)
        {
            return None;
        }
        let bus = u64::from(at.bus - self.start_bus);
        Some(
            (bus << 20)
                | (u64::from(at.device) << 15)
                | (u64::from(at.function) << 12)
                | u64::from(register),
        )
    }

    pub fn buses(&self) -> RangeInclusive<u8> {
        self.start_bus..=self.end_bus
    }
}

/// Why an MCFG was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McfgError {
    /// The allocations do not fill the table in 16-byte structures.
    Length(usize),
    /// Allocation `index` has an end bus below its start bus, a base not 1 MiB aligned, or
    /// a window that wraps.
    Allocation(usize),
    /// More allocations than the caller has room for.
    TooMany,
}

/// Offset of the first MCFG allocation (after the header and 8 reserved bytes).
const MCFG_ALLOCATIONS: usize = 44;

/// Lists the allocations of a checked MCFG (signature, length and checksum already
/// verified) into `out`; returns how many there are.
pub fn mcfg_allocations(mcfg: &[u8], out: &mut [Ecam]) -> Result<usize, McfgError> {
    let body = mcfg
        .get(MCFG_ALLOCATIONS..)
        .ok_or(McfgError::Length(mcfg.len()))?;
    let (chunks, rest) = body.as_chunks::<16>();
    if !rest.is_empty() {
        return Err(McfgError::Length(mcfg.len()));
    }
    if chunks.len() > out.len() {
        return Err(McfgError::TooMany);
    }
    for (i, (c, slot)) in chunks.iter().zip(out.iter_mut()).enumerate() {
        let [
            b0,
            b1,
            b2,
            b3,
            b4,
            b5,
            b6,
            b7,
            s0,
            s1,
            start_bus,
            end_bus,
            ..,
        ] = *c;
        let ecam = Ecam {
            base: u64::from_le_bytes([b0, b1, b2, b3, b4, b5, b6, b7]),
            segment: u16::from_le_bytes([s0, s1]),
            start_bus,
            end_bus,
        };
        if !ecam.base.is_multiple_of(1 << 20) || ecam.window().is_none() {
            return Err(McfgError::Allocation(i));
        }
        *slot = ecam;
    }
    Ok(chunks.len())
}

/// A function's identity and class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Function {
    pub at: Address,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    /// Header layout (bits 6:0 of the header type): 0 for an endpoint, 1 for a bridge.
    pub layout: u8,
    pub multifunction: bool,
}

impl Function {
    /// BARs in this header: six for an endpoint, two for a PCI-to-PCI bridge, none else.
    pub const fn bar_count(&self) -> u8 {
        match self.layout {
            0 => 6,
            1 => 2,
            _ => 0,
        }
    }
}

/// The function at `at`, or `None` if nothing answers there (vendor ID FFFFh).
pub fn read_function(cfg: &mut impl ConfigSpace, at: Address) -> Option<Function> {
    let id = cfg.read32(at, register::ID);
    let vendor = (id & 0xFFFF) as u16;
    if vendor == 0xFFFF || vendor == 0 {
        return None;
    }
    let class = cfg.read32(at, register::CLASS).to_le_bytes();
    let header = cfg.read32(at, register::HEADER).to_le_bytes();
    let [revision, prog_if, subclass, class] = class;
    let header_type = header[2];
    Some(Function {
        at,
        vendor,
        device: (id >> 16) as u16,
        class,
        subclass,
        prog_if,
        revision,
        layout: header_type & !MULTIFUNCTION,
        multifunction: header_type & MULTIFUNCTION != 0,
    })
}

/// Lists every function on `buses` into `out`; returns how many there are, or `Err` with
/// the count when `out` filled up. A device whose function 0 does not answer has no other
/// functions; functions 1–7 are probed only on multi-function devices.
pub fn enumerate(
    cfg: &mut impl ConfigSpace,
    buses: RangeInclusive<u8>,
    out: &mut [Function],
) -> Result<usize, usize> {
    let mut n = 0;
    for bus in buses {
        for device in 0..32 {
            let at = |function| Address {
                bus,
                device,
                function,
            };
            let Some(first) = read_function(cfg, at(0)) else {
                continue;
            };
            let functions = if first.multifunction { 8 } else { 1 };
            for function in 0..functions {
                let found = if function == 0 {
                    Some(first)
                } else {
                    read_function(cfg, at(function))
                };
                if let Some(f) = found {
                    *out.get_mut(n).ok_or(n)? = f;
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

/// Reads the byte at `offset` of configuration space through 32-bit accesses.
fn read8(cfg: &mut impl ConfigSpace, at: Address, offset: u16) -> u8 {
    let dword = cfg.read32(at, offset & !3);
    (dword >> (8 * u32::from(offset & 3))) as u8
}

/// The function's capabilities, as `(id, offset)` pairs in list order, into `out`; returns
/// how many. A pointer into the header (below 40h), a loop or more entries than `out`
/// holds ends the walk: configuration space is untrusted.
pub fn capabilities(cfg: &mut impl ConfigSpace, at: Address, out: &mut [(u8, u16)]) -> usize {
    if cfg.read32(at, register::COMMAND) & STATUS_CAPABILITIES == 0 {
        return 0;
    }
    let mut next = u16::from(read8(cfg, at, CAPABILITIES_POINTER)) & 0xFC;
    let mut n = 0;
    while next >= 0x40 && n < out.len() {
        // A loop revisits an offset: stop there.
        if out.iter().take(n).any(|&(_, seen)| seen == next) {
            break;
        }
        let id = read8(cfg, at, next);
        if let Some(slot) = out.get_mut(n) {
            *slot = (id, next);
        }
        n += 1;
        next = u16::from(read8(cfg, at, next + 1)) & 0xFC;
    }
    n
}

/// The offset of the first capability with `id`.
pub fn find_capability(cfg: &mut impl ConfigSpace, at: Address, id: u8) -> Option<u16> {
    let mut caps = [(0u8, 0u16); 48];
    let n = capabilities(cfg, at, &mut caps);
    caps.iter()
        .take(n)
        .find(|&&(c, _)| c == id)
        .map(|&(_, offset)| offset)
}

/// A function's MSI-X capability (PCI Express Base 6.x §7.7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msix {
    /// The capability's offset in configuration space.
    pub capability: u16,
    /// Table entries (1–2048).
    pub entries: u16,
    /// The BAR holding the table, and the table's offset in it (8-byte aligned).
    pub table_bar: u8,
    pub table_offset: u32,
    /// The same for the pending-bit array.
    pub pba_bar: u8,
    pub pba_offset: u32,
}

impl Msix {
    /// The physical address of the MSI-X table if it lies wholly inside `bar`, which must
    /// be the memory BAR it names; the table's size comes from the capability, the BAR's
    /// from sizing, and neither is trusted to fit the other.
    pub fn table_address(&self, bar: &Bar) -> Option<u64> {
        if bar.index != self.table_bar || bar.kind == BarKind::Io {
            return None;
        }
        let len = u64::from(self.entries).checked_mul(MSIX_ENTRY)?;
        let end = u64::from(self.table_offset).checked_add(len)?;
        if end > bar.size {
            return None;
        }
        let table = bar.address.checked_add(u64::from(self.table_offset))?;
        table.checked_add(len).map(|_| table)
    }
}

/// The last offset that holds a whole MSI-X capability: 12 bytes, ending at FFh.
const MSIX_CAPABILITY_END: u16 = 0x100 - 12;

/// Message control bits: MSI-X enable, and function mask.
pub const MSIX_ENABLE: u32 = 1 << 15;
pub const MSIX_FUNCTION_MASK: u32 = 1 << 14;
/// Bytes of one MSI-X table entry; the vector-control dword's mask bit.
pub const MSIX_ENTRY: u64 = 16;
pub const MSIX_MASKED: u32 = 1;

/// Reads the function's MSI-X capability, if it has a valid one: both BIRs name BARs 0–5,
/// and the 12-byte structure lies inside the 256 bytes that hold capabilities.
pub fn msix(cfg: &mut impl ConfigSpace, at: Address) -> Option<Msix> {
    let capability = find_capability(cfg, at, capability::MSIX)?;
    if capability > MSIX_CAPABILITY_END {
        return None;
    }
    let control = cfg.read32(at, capability) >> 16;
    let table = cfg.read32(at, capability + 4);
    let pba = cfg.read32(at, capability + 8);
    let (table_bar, pba_bar) = ((table & 7) as u8, (pba & 7) as u8);
    if table_bar > 5 || pba_bar > 5 {
        return None;
    }
    Some(Msix {
        capability,
        entries: ((control & 0x7FF) + 1) as u16,
        table_bar,
        table_offset: table & !7,
        pba_bar,
        pba_offset: pba & !7,
    })
}

/// The MSI or MSI-X message address that delivers to the local APIC with ID `apic_id` in
/// physical destination mode (Intel SDM Vol. 3A §11.11.1): `FEEh` in bits 31:20, the
/// destination in bits 19:12. IDs above 255 need interrupt remapping: `None`.
pub fn msi_address(apic_id: u32) -> Option<u64> {
    (apic_id <= 0xFF).then(|| 0xFEE0_0000 | (u64::from(apic_id) << 12))
}

/// The message data for a fixed, edge-triggered interrupt at `vector` (16–254).
pub fn msi_data(vector: u8) -> Option<u32> {
    (16..=254).contains(&vector).then_some(u32::from(vector))
}

/// What a BAR decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarKind {
    Memory32,
    Memory64,
    Io,
}

/// One implemented BAR, sized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    /// The BAR's index (a 64-bit BAR also uses the next one).
    pub index: u8,
    pub kind: BarKind,
    pub prefetchable: bool,
    /// The address the firmware assigned.
    pub address: u64,
    /// Bytes decoded: a power of two.
    pub size: u64,
}

/// The size a 32-bit memory BAR reports after all ones were written, or `None` if it is
/// not implemented (or reports a size that is not a power of two).
pub fn memory_size_32(mask: u32) -> Option<u64> {
    // An unimplemented BAR reads back zero: check before widening the mask.
    let m = mask & 0xFFFF_FFF0;
    if m == 0 {
        return None;
    }
    power_of_two(u64::from(m) | 0xFFFF_FFFF_0000_0000)
}

/// As [`memory_size_32`] for a 64-bit memory BAR's two halves.
pub fn memory_size_64(low: u32, high: u32) -> Option<u64> {
    power_of_two((u64::from(high) << 32) | u64::from(low & 0xFFFF_FFF0))
}

/// As [`memory_size_32`] for an I/O BAR. A BAR whose upper 16 bits read back zero decodes
/// only 16 address bits.
pub fn io_size(mask: u32) -> Option<u64> {
    let mut m = mask & 0xFFFF_FFFC;
    if m == 0 {
        return None;
    }
    if m & 0xFFFF_0000 == 0 {
        m |= 0xFFFF_0000;
    }
    power_of_two(u64::from(m) | 0xFFFF_FFFF_0000_0000)
}

/// The size a sizing mask encodes (its lowest set bit), if the mask is all ones above it.
fn power_of_two(mask: u64) -> Option<u64> {
    if mask == 0 {
        return None;
    }
    let size = (!mask).wrapping_add(1);
    (size != 0 && size.is_power_of_two() && mask == !(size - 1)).then_some(size)
}

/// Sizes every BAR of `f`, with I/O and memory decoding off meanwhile (so the device
/// never decodes the all-ones probe address), then restores every BAR and the command
/// register. Returns the implemented BARs.
pub fn size_bars(cfg: &mut impl ConfigSpace, f: &Function) -> [Option<Bar>; 6] {
    let mut bars = [None; 6];
    let command = cfg.read32(f.at, register::COMMAND) & 0xFFFF;
    // The upper half is the status register, whose bits clear when written as one: write
    // zeros there.
    cfg.write32(f.at, register::COMMAND, command & !DECODE);
    let mut i = 0u8;
    while i < f.bar_count() {
        let reg = register::BAR0 + 4 * u16::from(i);
        let original = cfg.read32(f.at, reg);
        cfg.write32(f.at, reg, u32::MAX);
        let mask = cfg.read32(f.at, reg);
        cfg.write32(f.at, reg, original);
        let (bar, used) = if original & 1 == 1 {
            let bar = io_size(mask).map(|size| Bar {
                index: i,
                kind: BarKind::Io,
                prefetchable: false,
                address: u64::from(original & 0xFFFF_FFFC),
                size,
            });
            (bar, 1)
        } else if (original >> 1) & 0b11 == 0b10 && i + 1 < f.bar_count() {
            let high_reg = reg + 4;
            let high = cfg.read32(f.at, high_reg);
            cfg.write32(f.at, high_reg, u32::MAX);
            let high_mask = cfg.read32(f.at, high_reg);
            cfg.write32(f.at, high_reg, high);
            let bar = memory_size_64(mask, high_mask).map(|size| Bar {
                index: i,
                kind: BarKind::Memory64,
                prefetchable: original & 0x8 != 0,
                address: (u64::from(high) << 32) | u64::from(original & 0xFFFF_FFF0),
                size,
            });
            (bar, 2)
        } else {
            let bar = memory_size_32(mask).map(|size| Bar {
                index: i,
                kind: BarKind::Memory32,
                prefetchable: original & 0x8 != 0,
                address: u64::from(original & 0xFFFF_FFF0),
                size,
            });
            (bar, 1)
        };
        if let Some(slot) = bars.get_mut(usize::from(i)) {
            *slot = bar;
        }
        i += used;
    }
    cfg.write32(f.at, register::COMMAND, command);
    bars
}

/// A short name for common classes (PCI Code and ID Assignment Specification), or `None`.
pub const fn class_name(class: u8, subclass: u8, prog_if: u8) -> Option<&'static str> {
    Some(match (class, subclass, prog_if) {
        (0x01, 0x06, 0x01) => "SATA controller (AHCI)",
        (0x01, 0x08, 0x02) => "NVMe controller",
        (0x01, 0x08, _) => "non-volatile memory controller",
        (0x01, _, _) => "storage controller",
        (0x02, 0x00, _) => "Ethernet controller",
        (0x02, 0x80, _) => "network controller",
        (0x03, 0x00, _) => "VGA-compatible display controller",
        (0x03, _, _) => "display controller",
        (0x04, 0x03, _) => "HD audio controller",
        (0x06, 0x00, _) => "host bridge",
        (0x06, 0x01, _) => "ISA bridge",
        (0x06, 0x04, _) => "PCI-to-PCI bridge",
        (0x06, _, _) => "bridge",
        // System peripheral, IOMMU (an AMD IOMMU's function, AMD IOMMU spec §3.1).
        (0x08, 0x06, _) => "IOMMU",
        (0x0C, 0x03, 0x30) => "USB controller (xHCI)",
        (0x0C, 0x03, _) => "USB controller",
        (0x0C, 0x05, _) => "SMBus controller",
        (0x0D, _, _) => "wireless controller",
        (0x12, _, _) => "processing accelerator",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A device model: configuration dwords, and per BAR register the bits it implements
    /// (writes keep only those; the low type bits are read-only).
    #[derive(Default)]
    struct Model {
        regs: BTreeMap<(Address, u16), u32>,
        /// (address, register) → (writable mask, read-only low bits).
        bars: BTreeMap<(Address, u16), (u32, u32)>,
        /// Set if a BAR was probed while decoding was on.
        probed_while_decoding: bool,
        status_writes_of_one: bool,
    }

    impl Model {
        fn add(&mut self, at: Address, vendor: u16, device: u16, class: [u8; 3], header: u8) {
            self.regs.insert(
                (at, register::ID),
                u32::from(vendor) | (u32::from(device) << 16),
            );
            let [class, subclass, prog_if] = class;
            self.regs.insert(
                (at, register::CLASS),
                u32::from_le_bytes([1, prog_if, subclass, class]),
            );
            self.regs
                .insert((at, register::HEADER), u32::from(header) << 16);
            // Decoding on, and a status bit (RW1C) pending.
            self.regs.insert((at, register::COMMAND), 0x0010_0007);
        }

        /// A BAR of `size` bytes at `address`, with type bits `low`.
        fn bar(&mut self, at: Address, index: u16, size: u64, address: u64, low: u32) {
            let reg = register::BAR0 + 4 * index;
            let mask = !(size - 1);
            self.bars
                .insert((at, reg), ((mask as u32) & !low_mask(low), low));
            self.regs.insert((at, reg), (address as u32 & !0xF) | low);
            if low & 0b110 == 0b100 {
                self.bars.insert((at, reg + 4), ((mask >> 32) as u32, 0));
                self.regs.insert((at, reg + 4), (address >> 32) as u32);
            }
        }
    }

    fn low_mask(low: u32) -> u32 {
        if low & 1 == 1 { 0x3 } else { 0xF }
    }

    impl ConfigSpace for Model {
        fn read32(&mut self, at: Address, register: u16) -> u32 {
            self.regs
                .get(&(at, register))
                .copied()
                .unwrap_or(if register == 0 { 0xFFFF_FFFF } else { 0 })
        }

        fn write32(&mut self, at: Address, register: u16, value: u32) {
            if register == register::COMMAND {
                if value & 0xFFFF_0000 != 0 {
                    self.status_writes_of_one = true;
                }
                let status = self.regs.get(&(at, register)).copied().unwrap_or(0) & 0xFFFF_0000;
                self.regs.insert((at, register), status | (value & 0xFFFF));
                return;
            }
            if let Some(&(mask, low)) = self.bars.get(&(at, register)) {
                let decoding = self.read32(at, register::COMMAND) & DECODE != 0;
                if value == u32::MAX && decoding {
                    self.probed_while_decoding = true;
                }
                self.regs.insert((at, register), (value & mask) | low);
            }
        }
    }

    fn at(bus: u8, device: u8, function: u8) -> Address {
        Address {
            bus,
            device,
            function,
        }
    }

    /// QEMU's q35 machine, roughly: a host bridge, a VGA device, a multi-function LPC
    /// device, an NVMe controller with a 64-bit BAR, a bridge, and a device on bus 1.
    fn q35() -> Model {
        let mut m = Model::default();
        m.add(at(0, 0, 0), 0x8086, 0x29C0, [0x06, 0x00, 0x00], 0);
        m.add(at(0, 1, 0), 0x1234, 0x1111, [0x03, 0x00, 0x00], 0);
        m.bar(at(0, 1, 0), 0, 16 << 20, 0xC000_0000, 0x8); // prefetchable
        m.bar(at(0, 1, 0), 2, 4096, 0xC100_0000, 0);
        m.add(at(0, 4, 0), 0x1B36, 0x0010, [0x01, 0x08, 0x02], 0);
        m.bar(at(0, 4, 0), 0, 16 << 10, 0x8_0000_0000, 0b100);
        m.add(
            at(0, 0x1F, 0),
            0x8086,
            0x2918,
            [0x06, 0x01, 0x00],
            MULTIFUNCTION,
        );
        m.add(at(0, 0x1F, 2), 0x8086, 0x2922, [0x01, 0x06, 0x01], 0);
        m.bar(at(0, 0x1F, 2), 4, 32, 0xC040, 0x1); // I/O
        m.bar(at(0, 0x1F, 2), 5, 4096, 0xC100_1000, 0);
        m.add(at(0, 0x1F, 3), 0x8086, 0x2930, [0x0C, 0x05, 0x00], 0);
        m.add(at(0, 0x1E, 0), 0x8086, 0x244E, [0x06, 0x04, 0x01], 1);
        m.add(at(1, 0, 0), 0x8086, 0x10D3, [0x02, 0x00, 0x00], 0);
        // Function 3 of a single-function device answers but must not be probed.
        m.add(at(0, 2, 3), 0xDEAD, 0xBEEF, [0xFF, 0, 0], 0);
        m
    }

    #[test]
    fn enumeration_finds_every_function_and_only_those() {
        let mut m = q35();
        let mut out = [Function::default(); 16];
        let n = enumerate(&mut m, 0..=255, &mut out).unwrap();
        let found: Vec<_> = out[..n]
            .iter()
            .map(|f| (f.at, f.vendor, f.device))
            .collect();
        assert_eq!(
            found,
            [
                (at(0, 0, 0), 0x8086, 0x29C0),
                (at(0, 1, 0), 0x1234, 0x1111),
                (at(0, 4, 0), 0x1B36, 0x0010),
                (at(0, 0x1E, 0), 0x8086, 0x244E),
                (at(0, 0x1F, 0), 0x8086, 0x2918),
                (at(0, 0x1F, 2), 0x8086, 0x2922),
                (at(0, 0x1F, 3), 0x8086, 0x2930),
                (at(1, 0, 0), 0x8086, 0x10D3),
            ]
        );
        let nvme = out[2];
        assert_eq!((nvme.class, nvme.subclass, nvme.prog_if), (1, 8, 2));
        assert_eq!(class_name(1, 8, 2), Some("NVMe controller"));
        assert_eq!(class_name(8, 6, 0), Some("IOMMU"));
        assert_eq!(out[3].layout, 1, "a bridge");
        assert!(out[4].multifunction);
        let mut small = [Function::default(); 3];
        assert_eq!(enumerate(&mut m, 0..=255, &mut small), Err(3));
        assert_eq!(format!("{}", at(0, 0x1F, 2)), "00:1f.2");
    }

    #[test]
    fn bars_are_sized_with_decoding_off_and_restored() {
        let mut m = q35();
        let before = m.regs.clone();
        let vga = read_function(&mut m, at(0, 1, 0)).unwrap();
        let bars = size_bars(&mut m, &vga);
        assert_eq!(
            bars[0],
            Some(Bar {
                index: 0,
                kind: BarKind::Memory32,
                prefetchable: true,
                address: 0xC000_0000,
                size: 16 << 20
            })
        );
        assert_eq!(bars[1], None, "not implemented");
        assert_eq!(
            bars[2].map(|b| (b.size, b.address)),
            Some((4096, 0xC100_0000))
        );
        let nvme = read_function(&mut m, at(0, 4, 0)).unwrap();
        let bars = size_bars(&mut m, &nvme);
        assert_eq!(
            bars[0],
            Some(Bar {
                index: 0,
                kind: BarKind::Memory64,
                prefetchable: false,
                address: 0x8_0000_0000,
                size: 16 << 10
            })
        );
        assert_eq!(bars[1], None, "the upper half of BAR 0");
        let ahci = read_function(&mut m, at(0, 0x1F, 2)).unwrap();
        let bars = size_bars(&mut m, &ahci);
        assert_eq!(
            bars[4].map(|b| (b.kind, b.size, b.address)),
            Some((BarKind::Io, 32, 0xC040))
        );
        assert_eq!(bars[5].map(|b| b.size), Some(4096));
        // Nothing changed, decoding was off while probing, and no status bit was cleared.
        assert_eq!(m.regs, before);
        assert!(!m.probed_while_decoding);
        assert!(!m.status_writes_of_one);
    }

    #[test]
    fn capability_lists_are_walked_safely() {
        let f = at(0, 4, 0);
        let mut m = q35();
        // Capabilities present; the list: 40h power management, 50h MSI-X (64 entries,
        // table in BAR 0 at 2000h, PBA in BAR 0 at 3000h), 60h PCI Express.
        let command = m.regs[&(f, register::COMMAND)];
        m.regs
            .insert((f, register::COMMAND), command | STATUS_CAPABILITIES);
        m.regs.insert((f, 0x34), 0x40);
        m.regs.insert((f, 0x40), 0x0003_5001);
        m.regs.insert((f, 0x50), (63 << 16) | 0x6011);
        m.regs.insert((f, 0x54), 0x2000);
        m.regs.insert((f, 0x58), 0x3000);
        m.regs.insert((f, 0x60), 0x0002_0010);
        let mut caps = [(0, 0); 8];
        let n = capabilities(&mut m, f, &mut caps);
        assert_eq!(&caps[..n], &[(0x01, 0x40), (0x11, 0x50), (0x10, 0x60)]);
        assert_eq!(
            msix(&mut m, f),
            Some(Msix {
                capability: 0x50,
                entries: 64,
                table_bar: 0,
                table_offset: 0x2000,
                pba_bar: 0,
                pba_offset: 0x3000
            })
        );
        assert_eq!(find_capability(&mut m, f, capability::MSI), None);
        // A loop back to 40h ends the walk.
        m.regs.insert((f, 0x60), 0x0002_4010);
        assert_eq!(capabilities(&mut m, f, &mut caps), 3);
        // A pointer into the header ends it too.
        m.regs.insert((f, 0x50), (63 << 16) | 0x2011);
        assert_eq!(capabilities(&mut m, f, &mut caps), 2);
        // A BIR naming no BAR is refused.
        m.regs.insert((f, 0x50), (63 << 16) | 0x0011);
        m.regs.insert((f, 0x54), 0x2006);
        assert_eq!(msix(&mut m, f), None);
        // Without the status bit there is no list at all.
        m.regs
            .insert((f, register::COMMAND), command & !STATUS_CAPABILITIES);
        assert_eq!(capabilities(&mut m, f, &mut caps), 0);
        // Messages.
        assert_eq!(msi_address(1), Some(0xFEE0_1000));
        assert_eq!(msi_address(255), Some(0xFEEF_F000));
        assert_eq!(msi_address(256), None);
        assert_eq!(
            (msi_data(0x40), msi_data(15), msi_data(255)),
            (Some(0x40), None, None)
        );
    }

    /// An MSI-X capability placed so that its 12 bytes cross the end of the 256-byte
    /// capability area is refused; its dwords at 100h and up belong to something else.
    #[test]
    fn an_msix_capability_must_fit_in_the_capability_area() {
        let f = at(0, 4, 0);
        let mut m = q35();
        let command = m.regs[&(f, register::COMMAND)];
        m.regs
            .insert((f, register::COMMAND), command | STATUS_CAPABILITIES);
        for (offset, accepted) in [(0xF4u16, true), (0xF8, false), (0xFC, false)] {
            m.regs.insert((f, 0x34), u32::from(offset));
            m.regs.insert((f, offset), (63 << 16) | 0x0011);
            m.regs.insert((f, offset + 4), 0x2000);
            m.regs.insert((f, offset + 8), 0x3000);
            assert_eq!(msix(&mut m, f).is_some(), accepted, "at {offset:#x}");
            m.regs.remove(&(f, offset));
        }
    }

    /// The table must lie inside the memory BAR the capability names: an I/O BAR's "address"
    /// is a port number, so writing MSI-X entries there would land in RAM.
    #[test]
    fn the_msix_table_is_located_only_inside_its_memory_bar() {
        let m = Msix {
            capability: 0x50,
            entries: 64,
            table_bar: 1,
            table_offset: 0x1000,
            pba_bar: 1,
            pba_offset: 0x2000,
        };
        let bar = Bar {
            index: 1,
            kind: BarKind::Memory64,
            prefetchable: false,
            address: 0xC000_0000,
            size: 0x4000,
        };
        assert_eq!(m.table_address(&bar), Some(0xC000_1000));
        // 64 entries of 16 bytes end at 1400h: inside a 2 KiB-offset 5 KiB window only.
        let exact = Bar {
            size: 0x1400,
            ..bar
        };
        assert_eq!(m.table_address(&exact), Some(0xC000_1000));
        let short = Bar {
            size: 0x13FF,
            ..bar
        };
        assert_eq!(m.table_address(&short), None, "one byte short");
        let io = Bar {
            kind: BarKind::Io,
            ..bar
        };
        assert_eq!(m.table_address(&io), None, "an I/O BAR");
        let other = Bar { index: 2, ..bar };
        assert_eq!(m.table_address(&other), None, "the wrong BAR");
        let wraps = Bar {
            address: u64::MAX - 0xFFF,
            ..bar
        };
        assert_eq!(m.table_address(&wraps), None, "wraps the address space");
    }

    #[test]
    fn sizing_masks() {
        assert_eq!(memory_size_32(0xFFFF_F000), Some(4096));
        assert_eq!(
            memory_size_32(0xFF00_0008),
            Some(16 << 20),
            "type bits ignored"
        );
        assert_eq!(memory_size_32(0), None, "not implemented");
        assert_eq!(
            memory_size_32(0xFFF0_F000),
            None,
            "not all ones above the size"
        );
        assert_eq!(memory_size_64(0, 0xFFFF_FFF0), Some(1 << 36));
        assert_eq!(memory_size_64(0xFFFF_C004, 0xFFFF_FFFF), Some(16 << 10));
        assert_eq!(io_size(0xFFFF_FFE1), Some(32));
        assert_eq!(io_size(0x0000_FFE1), Some(32), "a 16-bit I/O decoder");
        assert_eq!(io_size(0x1), None);
    }

    /// Random MCFG bodies never panic, and every accepted window is aligned and inside the
    /// address space; random sizing masks give only powers of two.
    #[test]
    fn mutations_never_panic_and_accepted_windows_are_sane() {
        use crate::testing::{Rng, mutate};
        let mut rng = Rng::new(0x5043_4945);
        let mut good = vec![0u8; 44];
        for (base, start, end) in [(0xB000_0000u64, 0u8, 255u8), (0xE000_0000, 0x80, 0x8F)] {
            good.extend(base.to_le_bytes());
            good.extend([0, 0, start, end, 0, 0, 0, 0]);
        }
        let mut accepted = 0;
        for case in 0..crate::testing::cases(5000) {
            let mut t = good.clone();
            mutate(&mut t, &mut rng);
            let mut out = [Ecam::default(); 4];
            if let Ok(n) = mcfg_allocations(&t, &mut out) {
                accepted += 1;
                for e in &out[..n] {
                    let (start, len) = e.window().unwrap();
                    assert!(
                        e.base.is_multiple_of(1 << 20) && start.checked_add(len).is_some(),
                        "case {case}"
                    );
                    assert!(e.start_bus <= e.end_bus, "case {case}");
                }
            }
            let mask = rng.next() as u32;
            for size in [
                memory_size_32(mask),
                io_size(mask),
                memory_size_64(mask, rng.next() as u32),
            ]
            .into_iter()
            .flatten()
            {
                assert!(size.is_power_of_two(), "case {case}: {mask:#x}");
            }
        }
        assert!(accepted > 500, "{accepted}");
    }

    /// A function whose configuration space the soak chose. Every access is checked against
    /// the [`ConfigSpace`] contract; it can answer differently on every read, and it keeps
    /// the writes it saw.
    struct Hostile {
        /// The 4 KiB of configuration space.
        bytes: Vec<u8>,
        /// Each BAR's writable bits (what an all-ones probe reads back).
        masks: [u32; 6],
        /// When set, every read is XORed with noise: a device that changes its answers.
        noise: Option<crate::testing::Rng>,
        writes: Vec<(u16, u32)>,
    }

    impl Hostile {
        fn dword(&self, register: u16) -> u32 {
            let at = usize::from(register);
            u32::from_le_bytes(self.bytes[at..at + 4].try_into().unwrap())
        }

        fn set_dword(&mut self, register: u16, value: u32) {
            let at = usize::from(register);
            self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    impl ConfigSpace for Hostile {
        fn read32(&mut self, _at: Address, register: u16) -> u32 {
            assert!(
                register < 4096 && register.is_multiple_of(4),
                "read at {register:#x}"
            );
            let mut v = self.dword(register);
            if let Some(noise) = &mut self.noise {
                v ^= noise.edge_u32();
            }
            v
        }

        fn write32(&mut self, _at: Address, register: u16, value: u32) {
            assert!(
                register < 4096 && register.is_multiple_of(4),
                "write at {register:#x}"
            );
            self.writes.push((register, value));
            if (0x10..0x28).contains(&register) {
                let mask = self.masks[usize::from((register - 0x10) / 4)];
                self.set_dword(register, if value == u32::MAX { mask } else { value });
            } else if register == register::COMMAND {
                let status = self.dword(register) & 0xFFFF_0000;
                self.set_dword(register, status | (value & 0xFFFF));
            }
        }
    }

    /// A plausible endpoint: a capability list at 40h (MSI, MSI-X, PCI Express), a 64-bit
    /// BAR, a 32-bit BAR and an I/O BAR.
    fn hostile_base() -> (Vec<u8>, [u32; 6]) {
        let mut b = vec![0u8; 4096];
        let mut put = |at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(0x00, 0x0010_1B36);
        put(0x04, 0x0010_0007);
        put(0x08, 0x0108_0201);
        put(0x0C, 0x0000_0000);
        put(0x10, 0xC000_0004); // BAR 0-1: 64-bit
        put(0x14, 0x0000_0008);
        put(0x18, 0xC100_0000); // BAR 2: 32-bit
        put(0x1C, 0x0000_C041); // BAR 3: I/O
        put(0x34, 0x40);
        put(0x40, 0x0003_5005); // MSI, next 50h
        put(0x50, (63 << 16) | 0x6011); // MSI-X, 64 entries, next 60h
        put(0x54, 0x2000);
        put(0x58, 0x3004);
        put(0x60, 0x0002_0010); // PCI Express, end of list
        (
            b,
            [0xFFFF_C000, 0xFFFF_FFFF, 0xFFFF_F000, 0xFFFF_FFE1, 0, 0],
        )
    }

    /// Whatever a function's configuration space holds, and even if it changes between
    /// reads, the walks stay inside it, terminate and return only what they promise; BAR
    /// sizing still ends with the command register restored (CLAUDE.md §72).
    #[test]
    fn hostile_configuration_space_never_breaks_the_walks() {
        use crate::testing::{Rng, mutate_window};
        let (base, masks) = hostile_base();
        let at = at(0, 4, 0);
        let mut rng = Rng::new(0x4346_4731);
        let (mut functions, mut lists, mut with_msix, mut bars) = (0u32, 0u32, 0u32, 0u32);
        for case in 0..crate::testing::cases(12_000) {
            let mut bytes = base.clone();
            match case % 4 {
                0 => mutate_window(&mut bytes, 0..0x40, &mut rng),
                1 => mutate_window(&mut bytes, 0x34..0x100, &mut rng),
                // An MSI-X capability at any offset the list can name, as its head: at the
                // top of the area its 12 bytes must not count as present.
                2 => {
                    let offset = 0x40 + 4 * rng.below(48);
                    bytes[0x34] = offset as u8;
                    bytes[offset..offset + 4]
                        .copy_from_slice(&((63u32 << 16) | 0x11).to_le_bytes());
                }
                _ => {
                    // Reach the checks behind the header: keep the list intact and scribble
                    // over the capability bodies only.
                    mutate_window(&mut bytes, 0x40..0x100, &mut rng);
                    bytes[0x34] = 0x40;
                }
            }
            let mut masks = masks;
            for m in &mut masks {
                if rng.below(4) == 0 {
                    *m = rng.edge_u32();
                }
            }
            let noise = (case % 5 == 0).then(|| Rng::new(rng.next()));
            let steady = noise.is_none();
            let mut dev = Hostile {
                bytes,
                masks,
                noise,
                writes: Vec::new(),
            };

            let mut caps = [(0u8, 0u16); 64];
            let n = capabilities(&mut dev, at, &mut caps);
            assert!(n <= 48, "case {case}: {n} capabilities in 192 bytes");
            for (i, &(_, offset)) in caps[..n].iter().enumerate() {
                assert!((0x40..=0xFC).contains(&offset), "case {case}: {offset:#x}");
                assert!(offset.is_multiple_of(4), "case {case}: {offset:#x}");
                assert!(caps[..i].iter().all(|&(_, o)| o != offset), "case {case}");
            }
            if n > 0 {
                lists += 1;
            }
            let mut small = [(0u8, 0u16); 2];
            assert!(capabilities(&mut dev, at, &mut small) <= 2, "case {case}");

            for id in [
                capability::MSI,
                capability::MSIX,
                capability::PCI_EXPRESS,
                0xFF,
            ] {
                if let (Some(offset), true) = (find_capability(&mut dev, at, id), steady) {
                    assert!(caps[..n].contains(&(id, offset)), "case {case}");
                }
            }

            if let Some(m) = msix(&mut dev, at) {
                with_msix += 1;
                assert!((1..=2048).contains(&m.entries), "case {case}: {m:?}");
                assert!(m.table_bar <= 5 && m.pba_bar <= 5, "case {case}: {m:?}");
                assert!(m.table_offset.is_multiple_of(8), "case {case}: {m:?}");
                assert!(m.pba_offset.is_multiple_of(8), "case {case}: {m:?}");
                // The 12-byte capability is wholly inside the 256 bytes that hold it.
                assert!((0x40..=0xF4).contains(&m.capability), "case {case}: {m:?}");
                assert!(m.capability.is_multiple_of(4), "case {case}: {m:?}");
                for address in [rng.edge_u64(), 0xC000_0000] {
                    let size = 1u64 << rng.below(40);
                    let b = Bar {
                        index: m.table_bar,
                        kind: BarKind::Memory64,
                        prefetchable: false,
                        address,
                        size,
                    };
                    if let Some(table) = m.table_address(&b) {
                        let len = u64::from(m.entries) * MSIX_ENTRY;
                        assert!(
                            u64::from(m.table_offset) + len <= size,
                            "case {case}: {m:?}"
                        );
                        assert_eq!(table, address + u64::from(m.table_offset), "case {case}");
                        assert!(table.checked_add(len).is_some(), "case {case}");
                    }
                }
            }

            let Some(f) = read_function(&mut dev, at) else {
                continue;
            };
            functions += 1;
            assert!(f.vendor != 0 && f.vendor != 0xFFFF, "case {case}: {f:?}");
            assert_eq!(f.at, at, "case {case}");
            assert_eq!(
                f.bar_count(),
                match f.layout {
                    0 => 6,
                    1 => 2,
                    _ => 0,
                },
                "case {case}: {f:?}"
            );
            assert!(f.layout < 0x80, "case {case}: {f:?}");

            let before: Vec<u32> = (0..u16::from(f.bar_count()))
                .map(|i| dev.dword(register::BAR0 + 4 * i))
                .collect();
            dev.writes.clear();
            let sized = size_bars(&mut dev, &f);
            for (i, bar) in sized.iter().enumerate() {
                let Some(bar) = bar else { continue };
                bars += 1;
                assert_eq!(usize::from(bar.index), i, "case {case}: {bar:?}");
                assert!(bar.size.is_power_of_two(), "case {case}: {bar:?}");
                if bar.kind == BarKind::Memory64 {
                    assert!(i + 1 < usize::from(f.bar_count()), "case {case}: {bar:?}");
                    assert_eq!(sized[i + 1], None, "case {case}: {bar:?}");
                }
                assert!(
                    bar.kind != BarKind::Io || !bar.prefetchable,
                    "case {case}: {bar:?}"
                );
            }
            // Decoding is off for every probe, and the status half is written as zeros.
            let commands: Vec<u32> = dev
                .writes
                .iter()
                .filter(|&&(r, _)| r == register::COMMAND)
                .map(|&(_, v)| v)
                .collect();
            assert_eq!(commands.len(), 2, "case {case}: {commands:x?}");
            assert_eq!(commands[0], commands[1] & !DECODE, "case {case}");
            assert_eq!(commands[1] & 0xFFFF_0000, 0, "case {case}");
            assert_eq!(dev.writes.first().map(|w| w.0), Some(register::COMMAND));
            assert_eq!(dev.writes.last().map(|w| w.0), Some(register::COMMAND));
            // A steady device gets every BAR back as it was.
            if steady {
                for (i, value) in before.iter().enumerate() {
                    assert_eq!(
                        dev.dword(register::BAR0 + 4 * i as u16),
                        *value,
                        "case {case}"
                    );
                }
            }
        }
        assert!(
            functions > 3000 && lists > 3000 && with_msix > 1000 && bars > 3000,
            "{functions} functions, {lists} lists, {with_msix} with MSI-X, {bars} BARs"
        );
    }

    /// Enumeration over any answers stays in its buses and output, lists each function once
    /// and looks past function 0 only on a multi-function device.
    #[test]
    fn hostile_enumeration_stays_in_bounds() {
        use crate::testing::{Rng, mutate_window};
        let (base, masks) = hostile_base();
        let mut rng = Rng::new(0x454E_554D);
        let mut listed = 0u32;
        for case in 0..crate::testing::cases(1500) {
            let mut bytes = base.clone();
            mutate_window(&mut bytes, 0..0x10, &mut rng);
            let noise = (case % 2 == 0).then(|| Rng::new(rng.next()));
            let mut dev = Hostile {
                bytes,
                masks,
                noise,
                writes: Vec::new(),
            };
            let start = rng.byte();
            let end = start.saturating_add(rng.below(3) as u8);
            let mut out = [Function::default(); 32];
            let n = match enumerate(&mut dev, start..=end, &mut out) {
                Ok(n) => n,
                Err(n) => {
                    assert_eq!(n, out.len(), "case {case}");
                    n
                }
            };
            listed += n as u32;
            assert!(dev.writes.is_empty(), "case {case}: enumeration wrote");
            for (i, f) in out[..n].iter().enumerate() {
                assert!((start..=end).contains(&f.at.bus), "case {case}: {f:?}");
                assert!(f.at.device < 32 && f.at.function < 8, "case {case}: {f:?}");
                assert!(f.vendor != 0 && f.vendor != 0xFFFF, "case {case}: {f:?}");
                assert!(out[..i].iter().all(|g| g.at != f.at), "case {case}: {f:?}");
            }
        }
        assert!(listed > 1000, "{listed}");
    }

    /// The ECAM arithmetic: an offset is inside the window's bytes, for any window the MCFG
    /// parser can return and any address.
    #[test]
    fn ecam_offsets_stay_inside_the_window() {
        use crate::testing::Rng;
        let mut rng = Rng::new(0x4543_414D);
        for case in 0..crate::testing::cases(20_000) {
            let (start_bus, end_bus) = (rng.byte(), rng.byte());
            let ecam = Ecam {
                base: rng.edge_u64() & !0xF_FFFF,
                segment: rng.byte() as u16,
                start_bus,
                end_bus,
            };
            let at = Address {
                bus: rng.byte(),
                device: rng.below(40) as u8,
                function: rng.below(12) as u8,
            };
            let register = rng.next() as u16 & 0x1FFF;
            let Some((_, len)) = ecam.window() else {
                continue;
            };
            if let Some(offset) = ecam.offset(at, register) {
                assert!(offset + 4 <= len, "case {case}: {offset:#x} of {len:#x}");
                assert!(register.is_multiple_of(4) && register < 4096, "case {case}");
                assert!(ecam.buses().contains(&at.bus), "case {case}");
            }
        }
    }

    #[test]
    fn mcfg_allocations_are_checked() {
        let mut t = vec![0u8; 44];
        let alloc = |base: u64, seg: u16, start: u8, end: u8| {
            let mut a = base.to_le_bytes().to_vec();
            a.extend(seg.to_le_bytes());
            a.extend([start, end, 0, 0, 0, 0]);
            a
        };
        t.extend(alloc(0xB000_0000, 0, 0, 255));
        let mut out = [Ecam::default(); 2];
        assert_eq!(mcfg_allocations(&t, &mut out), Ok(1));
        let e = out[0];
        assert_eq!(e.window(), Some((0xB000_0000, 256 << 20)));
        assert_eq!(
            e.offset(at(0, 0x1F, 2), 0x10),
            Some((0x1F << 15) | (2 << 12) | 0x10)
        );
        assert_eq!(e.offset(at(0, 32, 0), 0), None);
        assert_eq!(e.offset(at(0, 0, 8), 0), None);
        assert_eq!(e.offset(at(0, 0, 0), 2), None, "misaligned");
        assert_eq!(e.offset(at(0, 0, 0), 4096), None);
        // A window starting at bus 0x80: offsets are relative to that bus.
        let high = Ecam {
            base: 0xE000_0000,
            segment: 1,
            start_bus: 0x80,
            end_bus: 0x8F,
        };
        assert_eq!(high.window(), Some((0xE800_0000, 16 << 20)));
        assert_eq!(high.offset(at(0x81, 0, 0), 0), Some(1 << 20));
        assert_eq!(high.offset(at(0x7F, 0, 0), 0), None);
        // Malformed tables.
        let mut bad = t.clone();
        bad.push(0);
        assert_eq!(mcfg_allocations(&bad, &mut out), Err(McfgError::Length(61)));
        let mut bad = vec![0u8; 44];
        bad.extend(alloc(0xB000_0000, 0, 9, 8));
        assert_eq!(
            mcfg_allocations(&bad, &mut out),
            Err(McfgError::Allocation(0))
        );
        let mut bad = vec![0u8; 44];
        bad.extend(alloc(0xB008_0000, 0, 0, 1));
        assert_eq!(
            mcfg_allocations(&bad, &mut out),
            Err(McfgError::Allocation(0)),
            "not 1 MiB aligned"
        );
        let mut bad = vec![0u8; 44];
        bad.extend(alloc(!0xF_FFFF, 0, 0, 255));
        assert_eq!(
            mcfg_allocations(&bad, &mut out),
            Err(McfgError::Allocation(0)),
            "wraps"
        );
        let mut three = t.clone();
        three.extend(alloc(0xC000_0000, 1, 0, 0));
        three.extend(alloc(0xD000_0000, 2, 0, 0));
        assert_eq!(mcfg_allocations(&three, &mut out), Err(McfgError::TooMany));
    }
}
