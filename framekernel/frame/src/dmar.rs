//! The ACPI DMA Remapping Reporting table, DMAR (CLAUDE.md §32 Intel VT-d, §58 DMA
//! protection): where the platform's VT-d remapping hardware units are, which devices each
//! one translates, and the memory some devices must keep reaching.
//!
//! Reference: Intel Virtualization Technology for Directed I/O Architecture Specification,
//! Rev 5.20 (D51397-019, 2026-05-14), chapter 8 (BIOS considerations: the DMAR table, its
//! remapping structures and their device scopes). ACPI 6.6 reserves the signature and defers
//! to that specification.
//!
//! The table is untrusted input. [`parse`] checks the whole table once — the header, then
//! every remapping structure and every device scope: lengths, alignment, placement, path
//! elements and the rules that make device ownership unambiguous — and refuses it at the
//! first fault, naming the offset. The views it returns are then walked without further
//! checks and cannot panic. Device resolution ([`Dmar::unit_for`]) reads bridge bus numbers
//! through a caller-supplied [`BridgeBuses`], so it is host-tested over a model of a PCI
//! hierarchy. Pure, no `unsafe`, no allocation.
#![forbid(unsafe_code)]

/// Offset of the first remapping structure: the 36-byte table header, the host address
/// width (1 byte), the flags (1 byte) and 10 reserved bytes.
const STRUCTURES: usize = 48;
/// Bytes of a remapping structure's header (type, length).
const STRUCTURE_HEADER: usize = 4;
/// Bytes of a device scope's fixed part, before its path.
const SCOPE_HEADER: usize = 6;
/// A 4 KiB page: register sets and reserved regions are aligned to it.
const PAGE: u64 = 4096;

/// Remapping structure types (VT-d chapter 8).
pub mod kind {
    /// DMA Remapping Hardware Unit Definition.
    pub const DRHD: u16 = 0;
    /// Reserved Memory Region Reporting.
    pub const RMRR: u16 = 1;
    /// Root Port ATS Capability Reporting.
    pub const ATSR: u16 = 2;
    /// Remapping Hardware Static Affinity.
    pub const RHSA: u16 = 3;
    /// ACPI Name-space Device Declaration.
    pub const ANDD: u16 = 4;
    /// SoC Integrated Address Translation Cache.
    pub const SATC: u16 = 5;
}

/// The DMAR header's flags.
pub mod flags {
    /// The platform supports interrupt remapping.
    pub const INTR_REMAP: u8 = 1 << 0;
    /// The firmware asks the OS not to enable x2APIC mode (only with interrupt remapping).
    pub const X2APIC_OPT_OUT: u8 = 1 << 1;
    /// The platform opts in to DMA protection from boot (the units may already translate).
    pub const DMA_CTRL_PLATFORM_OPT_IN: u8 = 1 << 2;
    /// The firmware asks the OS not to enable DMA remapping.
    pub const DMA_REMAP_OPT_OUT: u8 = 1 << 3;
}

/// Why a DMAR was refused. Offsets are from the start of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmarError {
    /// The table is shorter than its 48-byte header (the length given).
    Short(usize),
    /// The host address width (in bits) is outside 32–64.
    HostAddressWidth(u16),
    /// The remapping structure at this offset is truncated, shorter than its type's minimum,
    /// or runs past the table.
    Structure(usize),
    /// The DRHD at this offset has a register base that is zero or not 4 KiB aligned, or the
    /// same base as an earlier unit.
    RegisterBase(usize),
    /// The RMRR at this offset has a base or end that is not on a 4 KiB boundary, or ends
    /// before it starts.
    ReservedRange(usize),
    /// The device scope at this offset is shorter than its header, runs past its structure,
    /// has an odd or empty path, names a device above 31 or a function above 7, or has a type
    /// its structure may not hold.
    Scope(usize),
    /// The DRHD at this offset is a second INCLUDE_PCI_ALL unit for its segment.
    IncludeAll(usize),
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

/// A PCI requester: the segment and the bus, device and function its requests carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Requester {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Requester {
    /// The 16-bit source ID the remapping hardware sees: bus, then device and function.
    pub fn source_id(&self) -> u16 {
        u16::from(self.bus) << 8 | u16::from(self.device & 0x1F) << 3 | u16::from(self.function & 7)
    }
}

/// What device resolution needs from configuration space.
pub trait BridgeBuses {
    /// The secondary and subordinate bus numbers of the PCI-to-PCI bridge at `bridge`, or
    /// `None` if that function is absent or is not a bridge.
    fn buses(&mut self, bridge: Requester) -> Option<(u8, u8)>;
}

/// A device scope's type (VT-d chapter 8, device scope structure).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// A PCI endpoint device (type 1).
    Endpoint,
    /// A PCI-to-PCI bridge and everything below it (type 2, a PCI sub-hierarchy).
    Bridge,
    /// An I/O APIC (type 3); the enumeration ID is its APIC ID.
    IoApic,
    /// An MSI-capable HPET (type 4); the enumeration ID is its block number.
    Hpet,
    /// An ACPI name-space device (type 5); the enumeration ID is its ANDD device number.
    Namespace,
    /// A type this parser does not know (kept, never matched).
    Unknown(u8),
}

impl ScopeKind {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Endpoint,
            2 => Self::Bridge,
            3 => Self::IoApic,
            4 => Self::Hpet,
            5 => Self::Namespace,
            other => Self::Unknown(other),
        }
    }

    fn is_pci(self) -> bool {
        matches!(self, Self::Endpoint | Self::Bridge)
    }
}

/// One device scope: a device named by a start bus and a path of (device, function) steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope<'a> {
    pub kind: ScopeKind,
    pub flags: u8,
    /// The I/O APIC ID, HPET number or ANDD device number (0 for PCI scopes).
    pub enumeration_id: u8,
    pub start_bus: u8,
    /// (device, function) pairs, two bytes each; checked non-empty and in range.
    path: &'a [u8],
}

impl<'a> Scope<'a> {
    /// The path's (device, function) steps: the first on `start_bus`, each later one on the
    /// secondary bus of the bridge before it.
    pub fn path(&self) -> impl Iterator<Item = (u8, u8)> + 'a {
        self.path.as_chunks::<2>().0.iter().map(|s| (s[0], s[1]))
    }

    /// How many steps the path has.
    pub fn path_len(&self) -> usize {
        self.path.len() / 2
    }

    /// The requester the path names in `segment`. A one-step path needs no configuration
    /// reads; each further step follows the secondary bus of the bridge before it, and the
    /// path does not resolve (`None`) if that function is not a bridge.
    pub fn resolve(&self, segment: u16, bridges: &mut impl BridgeBuses) -> Option<Requester> {
        let mut bus = self.start_bus;
        let mut at: Option<Requester> = None;
        for (device, function) in self.path() {
            if let Some(bridge) = at {
                bus = bridges.buses(bridge)?.0;
            }
            at = Some(Requester {
                segment,
                bus,
                device,
                function,
            });
        }
        at
    }

    /// Whether this PCI scope covers `r` (in the scope's own segment `segment`): an endpoint
    /// scope that resolves to `r`, or a bridge scope whose bridge is `r` or whose secondary to
    /// subordinate buses hold `r`'s bus. Other scope types cover no PCI requester.
    pub fn covers(&self, segment: u16, r: Requester, bridges: &mut impl BridgeBuses) -> bool {
        if segment != r.segment || !self.kind.is_pci() {
            return false;
        }
        let Some(named) = self.resolve(segment, bridges) else {
            return false;
        };
        if named == r {
            return true;
        }
        self.kind == ScopeKind::Bridge
            && bridges
                .buses(named)
                .is_some_and(|(secondary, subordinate)| (secondary..=subordinate).contains(&r.bus))
    }
}

/// The device scopes of one structure (already checked by [`parse`]).
#[derive(Clone, Copy, Debug)]
pub struct Scopes<'a> {
    bytes: &'a [u8],
}

impl<'a> Iterator for Scopes<'a> {
    type Item = Scope<'a>;

    fn next(&mut self) -> Option<Scope<'a>> {
        let len = usize::from(*self.bytes.get(1)?);
        // `parse` refused shorter scopes; stopping here also keeps a zero length from
        // looping for ever.
        if len < SCOPE_HEADER {
            self.bytes = &[];
            return None;
        }
        let entry = self.bytes.get(..len)?;
        self.bytes = self.bytes.get(len..).unwrap_or(&[]);
        Some(Scope {
            kind: ScopeKind::from_byte(*entry.first()?),
            flags: *entry.get(2)?,
            enumeration_id: *entry.get(4)?,
            start_bus: *entry.get(5)?,
            path: entry.get(SCOPE_HEADER..)?,
        })
    }
}

/// A DMA remapping hardware unit (DRHD).
#[derive(Clone, Copy, Debug)]
pub struct Unit<'a> {
    /// The structure's offset in the table.
    pub offset: usize,
    pub segment: u16,
    /// Physical address of its register set.
    pub register_base: u64,
    /// 4 KiB pages in its register set (2^N from the Size field).
    pub register_pages: u32,
    /// INCLUDE_PCI_ALL: the unit translates every PCI device of its segment that no other
    /// unit's scope lists.
    pub include_pci_all: bool,
    scopes: &'a [u8],
}

impl<'a> Unit<'a> {
    pub fn scopes(&self) -> Scopes<'a> {
        Scopes { bytes: self.scopes }
    }

    /// Whether this unit's scope lists `r` explicitly (an endpoint or a bridge above it).
    pub fn lists(&self, r: Requester, bridges: &mut impl BridgeBuses) -> bool {
        self.scopes().any(|s| s.covers(self.segment, r, bridges))
    }
}

/// A reserved memory region (RMRR) that the devices in its scope may access at any time,
/// so they must be able to reach it, identity-mapped, while translation is on.
#[derive(Clone, Copy, Debug)]
pub struct ReservedMemory<'a> {
    pub offset: usize,
    pub segment: u16,
    /// First byte (4 KiB aligned).
    pub base: u64,
    /// Last byte, inclusive (ends a 4 KiB page).
    pub limit: u64,
    scopes: &'a [u8],
}

impl<'a> ReservedMemory<'a> {
    pub fn scopes(&self) -> Scopes<'a> {
        Scopes { bytes: self.scopes }
    }

    /// Whether `r` is one of the devices that need this region.
    pub fn covers(&self, r: Requester, bridges: &mut impl BridgeBuses) -> bool {
        self.scopes().any(|s| s.covers(self.segment, r, bridges))
    }
}

/// A root-port ATS capability report (ATSR).
#[derive(Clone, Copy, Debug)]
pub struct AtsRoot<'a> {
    pub offset: usize,
    pub segment: u16,
    /// ALL_PORTS: every root port of the segment supports ATS.
    pub all_ports: bool,
    scopes: &'a [u8],
}

impl<'a> AtsRoot<'a> {
    pub fn scopes(&self) -> Scopes<'a> {
        Scopes { bytes: self.scopes }
    }
}

/// A remapping unit's NUMA proximity domain (RHSA).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Affinity {
    pub register_base: u64,
    pub proximity_domain: u32,
}

/// An ACPI name-space device declaration (ANDD): the device number a type-5 scope names,
/// and the device's ACPI object name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamespaceDevice<'a> {
    pub device_number: u8,
    /// The name's bytes, up to (not including) its NUL.
    pub name: &'a [u8],
}

/// One remapping structure.
#[derive(Clone, Copy, Debug)]
pub enum Structure<'a> {
    Unit(Unit<'a>),
    Reserved(ReservedMemory<'a>),
    AtsRoot(AtsRoot<'a>),
    Affinity(Affinity),
    NamespaceDevice(NamespaceDevice<'a>),
    /// A structure this parser does not decode (SATC and later types).
    Other {
        kind: u16,
        offset: usize,
    },
}

/// A checked DMAR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dmar<'a> {
    /// Physical addresses DMA can reach, in bits (the table's field plus one).
    pub host_address_width: u16,
    /// The header's flags ([`flags`]).
    pub flags: u8,
    /// The table from its first remapping structure on.
    body: &'a [u8],
}

/// The remapping structures of a checked DMAR.
#[derive(Clone, Copy, Debug)]
pub struct Structures<'a> {
    body: &'a [u8],
    at: usize,
}

impl<'a> Iterator for Structures<'a> {
    type Item = Structure<'a>;

    fn next(&mut self) -> Option<Structure<'a>> {
        let kind = u16_at(self.body, self.at)?;
        let len = usize::from(u16_at(self.body, self.at.checked_add(2)?)?);
        // `parse` refused shorter structures; this also keeps a zero length from looping.
        if len < STRUCTURE_HEADER {
            self.at = self.body.len();
            return None;
        }
        let s = self.body.get(self.at..self.at.checked_add(len)?)?;
        let offset = STRUCTURES + self.at;
        self.at += len;
        Some(match kind {
            kind::DRHD => Structure::Unit(Unit {
                offset,
                include_pci_all: *s.get(4)? & 1 != 0,
                register_pages: 1u32 << (*s.get(5)? & 0x0F),
                segment: u16_at(s, 6)?,
                register_base: u64_at(s, 8)?,
                scopes: s.get(16..)?,
            }),
            kind::RMRR => Structure::Reserved(ReservedMemory {
                offset,
                segment: u16_at(s, 6)?,
                base: u64_at(s, 8)?,
                limit: u64_at(s, 16)?,
                scopes: s.get(24..)?,
            }),
            kind::ATSR => Structure::AtsRoot(AtsRoot {
                offset,
                all_ports: *s.get(4)? & 1 != 0,
                segment: u16_at(s, 6)?,
                scopes: s.get(8..)?,
            }),
            kind::RHSA => Structure::Affinity(Affinity {
                register_base: u64_at(s, 8)?,
                proximity_domain: u32_at(s, 16)?,
            }),
            kind::ANDD => {
                let name = s.get(8..)?;
                let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                Structure::NamespaceDevice(NamespaceDevice {
                    device_number: *s.get(7)?,
                    name: name.get(..end)?,
                })
            }
            other => Structure::Other {
                kind: other,
                offset,
            },
        })
    }
}

impl<'a> Dmar<'a> {
    /// Every remapping structure, in table order.
    pub fn structures(&self) -> Structures<'a> {
        Structures {
            body: self.body,
            at: 0,
        }
    }

    /// The remapping hardware units, in table order.
    pub fn units(&self) -> impl Iterator<Item = Unit<'a>> + 'a {
        self.structures().filter_map(|s| match s {
            Structure::Unit(u) => Some(u),
            _ => None,
        })
    }

    /// The reserved memory regions, in table order.
    pub fn reserved_memory(&self) -> impl Iterator<Item = ReservedMemory<'a>> + 'a {
        self.structures().filter_map(|s| match s {
            Structure::Reserved(r) => Some(r),
            _ => None,
        })
    }

    /// Whether the platform supports interrupt remapping.
    pub fn interrupt_remapping(&self) -> bool {
        self.flags & flags::INTR_REMAP != 0
    }

    /// Whether the firmware asks the OS not to use x2APIC mode.
    pub fn x2apic_opt_out(&self) -> bool {
        self.flags & flags::X2APIC_OPT_OUT != 0
    }

    /// Whether the firmware asks the OS not to enable DMA remapping. The kernel reports this
    /// and does not honour it (CLAUDE.md §58).
    pub fn dma_remap_opt_out(&self) -> bool {
        self.flags & flags::DMA_REMAP_OPT_OUT != 0
    }

    /// Whether the platform opted in to DMA protection from boot.
    pub fn dma_control_opt_in(&self) -> bool {
        self.flags & flags::DMA_CTRL_PLATFORM_OPT_IN != 0
    }

    /// The unit that translates `r`'s requests: one whose scope lists it (an endpoint scope
    /// naming it first, then a bridge scope above it), else its segment's INCLUDE_PCI_ALL
    /// unit. `None`: no unit translates `r`, so its DMA is not remapped.
    pub fn unit_for(&self, r: Requester, bridges: &mut impl BridgeBuses) -> Option<Unit<'a>> {
        let named = |u: &Unit<'a>, kind: ScopeKind, bridges: &mut _| {
            u.segment == r.segment
                && u.scopes()
                    .filter(|s| s.kind == kind)
                    .any(|s| s.covers(u.segment, r, bridges))
        };
        self.units()
            .find(|u| named(u, ScopeKind::Endpoint, bridges))
            .or_else(|| self.units().find(|u| named(u, ScopeKind::Bridge, bridges)))
            .or_else(|| {
                self.units()
                    .find(|u| u.include_pci_all && u.segment == r.segment)
            })
    }

    /// The unit that remaps the interrupts of the I/O APIC whose ID is `id` (its MADT I/O
    /// APIC ID, the scope's enumeration ID), and the requester its interrupt messages carry —
    /// the source ID an IRTE for one of its pins must check. VT-d chapter 8: an I/O APIC is
    /// listed explicitly in the device scope of the unit that remaps its interrupts;
    /// INCLUDE_PCI_ALL never covers one. Refused if no unit lists it, if more than one scope
    /// does (the source ID would be ambiguous), or if its scope's path does not resolve.
    pub fn ioapic(
        &self,
        id: u8,
        bridges: &mut impl BridgeBuses,
    ) -> Result<(Unit<'a>, Requester), IoApicScopeError> {
        let mut found = None;
        for u in self.units() {
            for s in u
                .scopes()
                .filter(|s| s.kind == ScopeKind::IoApic && s.enumeration_id == id)
            {
                if found.is_some() {
                    return Err(IoApicScopeError::ListedTwice);
                }
                found = Some((u, s));
            }
        }
        let (u, s) = found.ok_or(IoApicScopeError::NotListed)?;
        let r = s
            .resolve(u.segment, bridges)
            .ok_or(IoApicScopeError::Unresolved)?;
        Ok((u, r))
    }
}

/// Why [`Dmar::ioapic`] names no unit for an I/O APIC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoApicScopeError {
    /// No unit's device scope lists an I/O APIC with this ID, so its interrupts cannot be
    /// remapped.
    NotListed,
    /// More than one device scope lists it.
    ListedTwice,
    /// Its scope's path does not resolve (a middle step is not a bridge).
    Unresolved,
}

/// Checks a DMAR (the whole table, `acpi::check_table(b"DMAR")` already passed) and returns
/// a view of it. Every rule below is checked here, once:
/// - the header is complete and the host address width is 32–64 bits;
/// - every structure has a header, is at least its type's minimum length and ends inside
///   the table (unknown types are skipped by their length);
/// - a DRHD's register base is non-zero, 4 KiB aligned and not another unit's;
/// - at most one INCLUDE_PCI_ALL unit per segment, and its scopes name no PCI device (the
///   flag already covers them);
/// - an RMRR's range starts and ends on 4 KiB boundaries and does not end before it starts;
/// - every device scope is at least 6 bytes, ends inside its structure, and (for the known
///   types) has a non-empty, even-length path of devices 0–31 and functions 0–7.
pub fn parse(table: &[u8]) -> Result<Dmar<'_>, DmarError> {
    let body = table
        .get(STRUCTURES..)
        .ok_or(DmarError::Short(table.len()))?;
    let width = u16::from(table.get(36).copied().unwrap_or(0)) + 1;
    if !(32..=64).contains(&width) {
        return Err(DmarError::HostAddressWidth(width));
    }
    let dmar = Dmar {
        host_address_width: width,
        flags: table.get(37).copied().unwrap_or(0),
        body,
    };
    let mut at = 0usize;
    while at < body.len() {
        let offset = STRUCTURES + at;
        let bad = DmarError::Structure(offset);
        let kind = u16_at(body, at).ok_or(bad)?;
        let len = usize::from(u16_at(body, at + 2).ok_or(bad)?);
        let minimum = match kind {
            kind::DRHD => 16,
            kind::RMRR => 24,
            kind::ATSR | kind::ANDD | kind::SATC => 8,
            kind::RHSA => 20,
            _ => STRUCTURE_HEADER,
        };
        if len < minimum {
            return Err(bad);
        }
        let s = body.get(at..at + len).ok_or(bad)?;
        let scopes_from = match kind {
            kind::DRHD => Some(16),
            kind::RMRR => Some(24),
            kind::ATSR | kind::SATC => Some(8),
            _ => None,
        };
        if let Some(from) = scopes_from {
            // An INCLUDE_PCI_ALL unit lists only I/O APICs, HPETs and name-space devices.
            let no_pci = kind == kind::DRHD && s.get(4).is_some_and(|f| f & 1 != 0);
            check_scopes(s, from, offset, no_pci)?;
        }
        match kind {
            kind::DRHD => check_unit(&dmar, s, offset, at)?,
            kind::RMRR => {
                let base = u64_at(s, 8).ok_or(bad)?;
                let limit = u64_at(s, 16).ok_or(bad)?;
                if !base.is_multiple_of(PAGE) || limit & (PAGE - 1) != PAGE - 1 || limit < base {
                    return Err(DmarError::ReservedRange(offset));
                }
            }
            _ => {}
        }
        at += len;
    }
    Ok(dmar)
}

/// Checks the device scopes of the structure `s` (at table offset `offset`), which start at
/// `from` within it.
fn check_scopes(s: &[u8], from: usize, offset: usize, no_pci: bool) -> Result<(), DmarError> {
    let mut at = from;
    while at < s.len() {
        let bad = DmarError::Scope(offset + at);
        let kind = ScopeKind::from_byte(*s.get(at).ok_or(bad)?);
        let len = usize::from(*s.get(at + 1).ok_or(bad)?);
        if len < SCOPE_HEADER {
            return Err(bad);
        }
        let scope = s.get(at..at + len).ok_or(bad)?;
        let path = scope.get(SCOPE_HEADER..).unwrap_or(&[]);
        if !matches!(kind, ScopeKind::Unknown(_)) {
            let (steps, rest) = path.as_chunks::<2>();
            if steps.is_empty() || !rest.is_empty() {
                return Err(bad);
            }
            if steps
                .iter()
                .any(|&[device, function]| device > 31 || function > 7)
            {
                return Err(bad);
            }
        }
        if no_pci && kind.is_pci() {
            return Err(bad);
        }
        at += len;
    }
    Ok(())
}

/// Checks the DRHD `s` at table offset `offset` (body offset `at`) against the units before
/// it: its register base, and the one-INCLUDE_PCI_ALL-per-segment rule.
fn check_unit(dmar: &Dmar<'_>, s: &[u8], offset: usize, at: usize) -> Result<(), DmarError> {
    let base = u64_at(s, 8).ok_or(DmarError::Structure(offset))?;
    // The base must be aligned to the register set's size, 2^N pages (Size bits 3:0).
    let pages = 1u64 << (s.get(5).copied().unwrap_or(0) & 0x0F);
    if base == 0 || !base.is_multiple_of(PAGE * pages) {
        return Err(DmarError::RegisterBase(offset));
    }
    let include_all = s.get(4).is_some_and(|f| f & 1 != 0);
    let segment = u16_at(s, 6).ok_or(DmarError::Structure(offset))?;
    let earlier = Dmar {
        body: dmar.body.get(..at).unwrap_or(&[]),
        ..*dmar
    };
    for u in earlier.units() {
        if u.register_base == base {
            return Err(DmarError::RegisterBase(offset));
        }
        if include_all && u.include_pci_all && u.segment == segment {
            return Err(DmarError::IncludeAll(offset));
        }
    }
    Ok(())
}

/// Test builders, shared with the kernel-model tests elsewhere in the crate.
#[cfg(test)]
pub(crate) mod build {
    use alloc::vec::Vec;

    /// A device scope of `kind` with `id`, `bus` and a path of (device, function) steps.
    pub(crate) fn scope(kind: u8, id: u8, bus: u8, path: &[(u8, u8)]) -> Vec<u8> {
        let mut s = alloc::vec![kind, (6 + 2 * path.len()) as u8, 0, 0, id, bus];
        for &(d, f) in path {
            s.extend_from_slice(&[d, f]);
        }
        s
    }

    /// A DRHD: `flags` (bit 0 INCLUDE_PCI_ALL), `segment`, `base` and its scopes.
    pub(crate) fn drhd(flags: u8, segment: u16, base: u64, scopes: &[Vec<u8>]) -> Vec<u8> {
        let mut s = alloc::vec![0, 0, 0, 0, flags, 0];
        s.extend_from_slice(&segment.to_le_bytes());
        s.extend_from_slice(&base.to_le_bytes());
        for sc in scopes {
            s.extend_from_slice(sc);
        }
        let len = s.len() as u16;
        s[2..4].copy_from_slice(&len.to_le_bytes());
        s
    }

    /// An RMRR over `[base, limit]` for the devices in `scopes`.
    pub(crate) fn rmrr(segment: u16, base: u64, limit: u64, scopes: &[Vec<u8>]) -> Vec<u8> {
        let mut s = alloc::vec![1, 0, 0, 0, 0, 0];
        s.extend_from_slice(&segment.to_le_bytes());
        s.extend_from_slice(&base.to_le_bytes());
        s.extend_from_slice(&limit.to_le_bytes());
        for sc in scopes {
            s.extend_from_slice(sc);
        }
        let len = s.len() as u16;
        s[2..4].copy_from_slice(&len.to_le_bytes());
        s
    }

    /// A whole DMAR table (header, width field `haw`, `flags`, then `structures`), with a
    /// correct length and checksum.
    pub(crate) fn table(haw: u8, flags: u8, structures: &[Vec<u8>]) -> Vec<u8> {
        let mut t = alloc::vec![0u8; 48];
        t[..4].copy_from_slice(b"DMAR");
        t[8] = 1;
        t[36] = haw;
        t[37] = flags;
        for s in structures {
            t.extend_from_slice(s);
        }
        let len = t.len() as u32;
        t[4..8].copy_from_slice(&len.to_le_bytes());
        seal(&mut t);
        t
    }

    /// Recomputes the checksum byte (offset 9) so the table sums to zero.
    pub(crate) fn seal(t: &mut [u8]) {
        if t.len() > 9 {
            t[9] = 0;
            let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
            t[9] = 0u8.wrapping_sub(sum);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;
    use alloc::vec::Vec;
    use std::collections::BTreeMap;

    /// A PCI hierarchy model: bridges by location, with (secondary, subordinate).
    #[derive(Default)]
    struct Model(BTreeMap<(u16, u8, u8, u8), (u8, u8)>);

    impl BridgeBuses for Model {
        fn buses(&mut self, b: Requester) -> Option<(u8, u8)> {
            self.0
                .get(&(b.segment, b.bus, b.device, b.function))
                .copied()
        }
    }

    fn at(bus: u8, device: u8, function: u8) -> Requester {
        Requester {
            segment: 0,
            bus,
            device,
            function,
        }
    }

    /// The table QEMU 11.1 builds for q35 with `intremap=on` (hw/i386/acpi-build.c,
    /// `build_dmar_q35`): one unit at 0xfed90000 without INCLUDE_PCI_ALL, listing the I/O
    /// APIC on the pseudo bus 0xff and every PCI device on bus 0.
    fn qemu_q35() -> Vec<u8> {
        let mut scopes = alloc::vec![scope(3, 0, 0xFF, &[(0, 0)])];
        for (d, f) in [
            (0, 0),
            (1, 0),
            (2, 0),
            (3, 0),
            (0x1F, 0),
            (0x1F, 2),
            (0x1F, 3),
        ] {
            scopes.push(scope(1, 0, 0, &[(d, f)]));
        }
        table(47, flags::INTR_REMAP, &[drhd(0, 0, 0xFED9_0000, &scopes)])
    }

    #[test]
    fn qemu_table_parses_and_resolves_its_devices() {
        let t = qemu_q35();
        assert_eq!(crate::acpi::check_table(&t, b"DMAR"), Ok(()));
        let d = parse(&t).unwrap();
        assert_eq!(d.host_address_width, 48);
        assert!(d.interrupt_remapping() && !d.x2apic_opt_out() && !d.dma_control_opt_in());
        let units: Vec<_> = d.units().collect();
        assert_eq!(units.len(), 1);
        let u = units[0];
        assert_eq!((u.offset, u.segment, u.register_base), (48, 0, 0xFED9_0000));
        assert_eq!(u.register_pages, 1);
        assert!(!u.include_pci_all);
        let scopes: Vec<_> = u.scopes().collect();
        assert_eq!(scopes.len(), 8);
        assert_eq!(scopes[0].kind, ScopeKind::IoApic);
        let ioapic = scopes[0].resolve(0, &mut Model::default()).unwrap();
        assert_eq!(ioapic.source_id(), 0xFF00, "the I/O APIC's requester ID");
        let mut m = Model::default();
        let nvme = at(0, 2, 0);
        assert_eq!(nvme.source_id(), 0x0010);
        assert_eq!(
            d.unit_for(nvme, &mut m).map(|u| u.register_base),
            Some(0xFED9_0000)
        );
        assert_eq!(
            d.unit_for(at(0, 0x1F, 2), &mut m).map(|u| u.register_base),
            Some(0xFED9_0000)
        );
        // Not listed, and no INCLUDE_PCI_ALL unit: nothing translates it.
        assert!(d.unit_for(at(0, 4, 0), &mut m).is_none());
        assert!(d.unit_for(at(1, 0, 0), &mut m).is_none());
        assert_eq!(d.reserved_memory().count(), 0);
    }

    /// A two-unit platform: a graphics unit listing 00:02.0, a unit for a root port's
    /// sub-hierarchy, and an INCLUDE_PCI_ALL unit (last) listing the I/O APIC and the HPET;
    /// an RMRR for the USB controller, an ATSR, an RHSA, an ANDD and an unknown structure.
    fn platform(model: &mut Model) -> Vec<u8> {
        // Root port 00:1c.0 → bus 1, whose 01:00.0 is a switch → buses 2–3.
        model.0.insert((0, 0, 0x1C, 0), (1, 3));
        model.0.insert((0, 1, 0, 0), (2, 3));
        let gfx = drhd(0, 0, 0xFED9_0000, &[scope(1, 0, 0, &[(2, 0)])]);
        let port = drhd(0, 0, 0xFED9_2000, &[scope(2, 0, 0, &[(0x1C, 0)])]);
        let mut all = drhd(
            1,
            0,
            0xFED9_4000,
            &[
                scope(3, 2, 0xF0, &[(0x1F, 0)]),
                scope(4, 0, 0, &[(0x1F, 7)]),
            ],
        );
        all[5] = 1; // a register set of two pages
        let usb = rmrr(0, 0x7A00_0000, 0x7A0F_FFFF, &[scope(1, 0, 0, &[(0x14, 0)])]);
        let atsr = alloc::vec![2, 0, 8, 0, 1, 0, 0, 0];
        let mut rhsa = alloc::vec![3, 0, 20, 0, 0, 0, 0, 0];
        rhsa.extend_from_slice(&0xFED9_0000u64.to_le_bytes());
        rhsa.extend_from_slice(&1u32.to_le_bytes());
        let mut andd = alloc::vec![4, 0, 0, 0, 0, 0, 0, 5];
        andd.extend_from_slice(b"\\_SB.PCI0.I2C0\0");
        let len = andd.len() as u16;
        andd[2..4].copy_from_slice(&len.to_le_bytes());
        let unknown = alloc::vec![0x99, 0, 6, 0, 0xAA, 0xBB];
        table(38, 0b11, &[gfx, port, usb, atsr, rhsa, andd, unknown, all])
    }

    #[test]
    fn units_scopes_and_every_structure_type() {
        let mut m = Model::default();
        let t = platform(&mut m);
        let d = parse(&t).unwrap();
        assert_eq!(d.host_address_width, 39);
        assert!(d.interrupt_remapping() && d.x2apic_opt_out());
        let kinds: Vec<_> = d
            .structures()
            .map(|s| match s {
                Structure::Unit(_) => 0,
                Structure::Reserved(_) => 1,
                Structure::AtsRoot(a) => {
                    assert!(a.all_ports && a.scopes().count() == 0);
                    2
                }
                Structure::Affinity(a) => {
                    assert_eq!((a.register_base, a.proximity_domain), (0xFED9_0000, 1));
                    3
                }
                Structure::NamespaceDevice(n) => {
                    assert_eq!((n.device_number, n.name), (5, &b"\\_SB.PCI0.I2C0"[..]));
                    4
                }
                Structure::Other { kind, .. } => kind,
            })
            .collect();
        assert_eq!(kinds, [0, 0, 1, 2, 3, 4, 0x99, 0]);
        let all = d.units().last().unwrap();
        assert!(all.include_pci_all);
        assert_eq!(all.register_pages, 2);
        let s: Vec<_> = all.scopes().collect();
        assert_eq!((s[0].kind, s[0].enumeration_id), (ScopeKind::IoApic, 2));
        assert_eq!(s[1].kind, ScopeKind::Hpet);
        // Explicit endpoint scope.
        let mut unit = |r| d.unit_for(r, &mut m).map(|u| u.register_base);
        assert_eq!(unit(at(0, 2, 0)), Some(0xFED9_0000));
        // The root port itself, and devices on its secondary and subordinate buses.
        assert_eq!(unit(at(0, 0x1C, 0)), Some(0xFED9_2000));
        assert_eq!(unit(at(1, 0, 0)), Some(0xFED9_2000));
        assert_eq!(unit(at(3, 5, 1)), Some(0xFED9_2000));
        // Everything else on segment 0 falls to INCLUDE_PCI_ALL; segment 1 has no unit.
        assert_eq!(unit(at(0, 0x14, 0)), Some(0xFED9_4000));
        assert_eq!(unit(at(4, 0, 0)), Some(0xFED9_4000));
        assert_eq!(
            unit(Requester {
                segment: 1,
                ..at(0, 2, 0)
            }),
            None
        );
        // The RMRR names the USB controller only.
        let r = d.reserved_memory().next().unwrap();
        assert_eq!((r.base, r.limit), (0x7A00_0000, 0x7A0F_FFFF));
        assert!(r.covers(at(0, 0x14, 0), &mut m));
        assert!(!r.covers(at(0, 0x14, 1), &mut m));
    }

    #[test]
    fn multi_step_paths_follow_secondary_buses() {
        let mut m = Model::default();
        m.0.insert((0, 0, 0x1C, 0), (1, 3));
        m.0.insert((0, 1, 0, 0), (2, 3));
        // 00:1c.0 → 01:00.0 → 02:03.1
        let s = scope(1, 0, 0, &[(0x1C, 0), (0, 0), (3, 1)]);
        let t = table(47, 0, &[drhd(0, 0, 0x1000, &[s])]);
        let d = parse(&t).unwrap();
        let sc = d.units().next().unwrap().scopes().next().unwrap();
        assert_eq!(sc.path_len(), 3);
        assert_eq!(sc.resolve(0, &mut m), Some(at(2, 3, 1)));
        assert_eq!(
            d.unit_for(at(2, 3, 1), &mut m).unwrap().register_base,
            0x1000
        );
        assert!(d.unit_for(at(2, 3, 0), &mut m).is_none());
        // A middle step that is not a bridge: the path does not resolve, and covers nothing.
        m.0.remove(&(0, 1, 0, 0));
        assert_eq!(sc.resolve(0, &mut m), None);
        assert!(d.unit_for(at(2, 3, 1), &mut m).is_none());
    }

    #[test]
    fn io_apics_name_their_unit_and_source_id() {
        // QEMU's q35: I/O APIC 0 on the pseudo bus 0xff, source ID 0xff00.
        let t = qemu_q35();
        let d = parse(&t).unwrap();
        let mut none = Model::default();
        let (u, r) = d.ioapic(0, &mut none).unwrap();
        assert_eq!(u.register_base, 0xFED9_0000);
        assert_eq!(r, at(0xFF, 0, 0));
        assert_eq!(r.source_id(), 0xFF00);
        assert_eq!(
            d.ioapic(1, &mut none).err(),
            Some(IoApicScopeError::NotListed)
        );
        // The platform: I/O APIC 2 is listed by the INCLUDE_PCI_ALL unit (bus 0xf0, 1f.0).
        // INCLUDE_PCI_ALL does not cover I/O APICs, and an HPET scope with the same
        // enumeration ID is not one.
        let mut m = Model::default();
        let t = platform(&mut m);
        let d = parse(&t).unwrap();
        let (u, r) = d.ioapic(2, &mut m).unwrap();
        assert!(u.include_pci_all);
        assert_eq!((u.register_base, r.source_id()), (0xFED9_4000, 0xF0F8));
        assert_eq!(d.ioapic(0, &mut m).err(), Some(IoApicScopeError::NotListed));
        // Listed by two units, or twice by one: the source ID would be ambiguous.
        let one = scope(3, 0, 0xFF, &[(0, 0)]);
        let other = scope(3, 0, 0xFE, &[(0, 0)]);
        let t = table(
            47,
            flags::INTR_REMAP,
            &[
                drhd(0, 0, 0x1000, core::slice::from_ref(&one)),
                drhd(0, 0, 0x2000, core::slice::from_ref(&other)),
            ],
        );
        let d = parse(&t).unwrap();
        assert_eq!(
            d.ioapic(0, &mut none).err(),
            Some(IoApicScopeError::ListedTwice)
        );
        let t = table(
            47,
            flags::INTR_REMAP,
            &[drhd(0, 0, 0x1000, &[one.clone(), one])],
        );
        assert_eq!(
            parse(&t).unwrap().ioapic(0, &mut none).err(),
            Some(IoApicScopeError::ListedTwice)
        );
        // An I/O APIC behind a bridge: the path follows its secondary bus; without the bridge
        // it does not resolve.
        let behind = scope(3, 4, 0, &[(0x1C, 0), (0, 0)]);
        let t = table(47, flags::INTR_REMAP, &[drhd(0, 0, 0x1000, &[behind])]);
        let d = parse(&t).unwrap();
        let mut bridged = Model::default();
        bridged.0.insert((0, 0, 0x1C, 0), (5, 5));
        assert_eq!(d.ioapic(4, &mut bridged).unwrap().1.source_id(), 0x0500);
        assert_eq!(
            d.ioapic(4, &mut none).err(),
            Some(IoApicScopeError::Unresolved)
        );
    }

    #[test]
    fn malformed_tables_are_refused_at_their_offset() {
        let ok = scope(1, 0, 0, &[(2, 0)]);
        let unit = |s: &[Vec<u8>]| drhd(0, 0, 0x1000, s);
        // The header.
        assert_eq!(parse(&[0u8; 47]), Err(DmarError::Short(47)));
        assert_eq!(
            parse(&table(30, 0, &[])),
            Err(DmarError::HostAddressWidth(31))
        );
        assert_eq!(
            parse(&table(64, 0, &[])),
            Err(DmarError::HostAddressWidth(65))
        );
        assert!(parse(&table(31, 0, &[])).is_ok());
        assert!(parse(&table(63, 0, &[])).is_ok());
        // Structure headers and lengths.
        let mut t = table(47, 0, &[]);
        t.extend_from_slice(&[0, 0, 0]);
        assert_eq!(parse(&t), Err(DmarError::Structure(48)));
        let mut short = unit(&[]);
        short[2] = 12;
        assert_eq!(
            parse(&table(47, 0, &[short.clone()])),
            Err(DmarError::Structure(48))
        );
        short[2] = 200;
        assert_eq!(
            parse(&table(47, 0, &[short])),
            Err(DmarError::Structure(48))
        );
        let zero = alloc::vec![0x77, 0, 0, 0];
        assert_eq!(parse(&table(47, 0, &[zero])), Err(DmarError::Structure(48)));
        let mut rhsa = alloc::vec![3u8, 0, 16, 0];
        rhsa.extend_from_slice(&[0; 12]);
        assert_eq!(parse(&table(47, 0, &[rhsa])), Err(DmarError::Structure(48)));
        // Register bases.
        for base in [0, 0x1001, 0xFED9_0800] {
            assert_eq!(
                parse(&table(
                    47,
                    0,
                    &[drhd(0, 0, base, core::slice::from_ref(&ok))]
                )),
                Err(DmarError::RegisterBase(48)),
                "{base:#x}"
            );
        }
        // A two-page register set must be aligned to 8 KiB.
        let two_pages = |base: u64| {
            let mut s = drhd(0, 0, base, core::slice::from_ref(&ok));
            s[5] = 1;
            s
        };
        assert_eq!(
            parse(&table(47, 0, &[two_pages(0xFED9_1000)])),
            Err(DmarError::RegisterBase(48))
        );
        assert!(parse(&table(47, 0, &[two_pages(0xFED9_2000)])).is_ok());
        let a = unit(core::slice::from_ref(&ok));
        let second = 48 + a.len();
        assert_eq!(
            parse(&table(47, 0, &[a.clone(), a.clone()])),
            Err(DmarError::RegisterBase(second))
        );
        // One INCLUDE_PCI_ALL unit per segment, naming no PCI device.
        let all1 = drhd(1, 0, 0x2000, &[]);
        let all2 = drhd(1, 0, 0x3000, &[]);
        assert_eq!(
            parse(&table(47, 0, &[all1.clone(), all2])),
            Err(DmarError::IncludeAll(48 + all1.len()))
        );
        assert!(parse(&table(47, 0, &[all1, drhd(1, 1, 0x3000, &[])])).is_ok());
        assert_eq!(
            parse(&table(
                47,
                0,
                &[drhd(1, 0, 0x2000, core::slice::from_ref(&ok))]
            )),
            Err(DmarError::Scope(64))
        );
        // Device scopes.
        let bad_scopes: [Vec<u8>; 6] = [
            alloc::vec![1, 5, 0, 0, 0],           // below the 6-byte header
            alloc::vec![1, 6, 0, 0, 0, 0],        // an empty path
            alloc::vec![1, 7, 0, 0, 0, 0, 2],     // an odd path
            alloc::vec![1, 10, 0, 0, 0, 0, 2, 0], // runs past the structure
            scope(1, 0, 0, &[(32, 0)]),           // device 32
            scope(2, 0, 0, &[(2, 0), (0, 8)]),    // function 8
        ];
        for s in bad_scopes {
            assert_eq!(
                parse(&table(47, 0, &[unit(&[ok.clone(), s.clone()])])),
                Err(DmarError::Scope(48 + 16 + ok.len())),
                "{s:?}"
            );
        }
        // An unknown scope type only needs its header to fit.
        assert!(parse(&table(47, 0, &[unit(&[alloc::vec![9, 6, 0, 0, 0, 0]])])).is_ok());
        // Reserved ranges.
        for (base, limit) in [
            (0x1000_0800, 0x1000_FFFF),
            (0x1000_0000, 0x1000_FFFE),
            (0x2000_0000, 0x1FFF_FFFF),
        ] {
            assert_eq!(
                parse(&table(
                    47,
                    0,
                    &[rmrr(0, base, limit, core::slice::from_ref(&ok))]
                )),
                Err(DmarError::ReservedRange(48)),
                "{base:#x}..{limit:#x}"
            );
        }
        assert!(parse(&table(47, 0, &[rmrr(0, 0x1000, 0x1FFF, &[ok])])).is_ok());
    }

    /// Mutated tables never panic the parser or the resolver, and an accepted table keeps
    /// every invariant `parse` promises (CLAUDE.md §72).
    #[test]
    fn mutations_never_panic_and_accepted_tables_are_consistent() {
        use crate::testing::{Rng, mutate};
        let mut model = Model::default();
        let bases = [qemu_q35(), platform(&mut model)];
        let mut rng = Rng::new(0xD3A2);
        let mut accepted = 0u32;
        for case in 0..crate::testing::cases(12_000) {
            let mut t = bases[case % 2].clone();
            mutate(&mut t, &mut rng);
            if case % 3 != 0 && t.len() >= 36 {
                let len = t.len() as u32;
                t[4..8].copy_from_slice(&len.to_le_bytes());
                seal(&mut t);
            }
            let Ok(d) = parse(&t) else { continue };
            accepted += 1;
            assert!((32..=64).contains(&d.host_address_width), "case {case}");
            let mut include_all = Vec::new();
            for u in d.units() {
                assert!(
                    u.register_base != 0 && u.register_base.is_multiple_of(4096),
                    "case {case}"
                );
                if u.include_pci_all {
                    assert!(!include_all.contains(&u.segment), "case {case}");
                    include_all.push(u.segment);
                    assert!(u.scopes().all(|s| !s.kind.is_pci()), "case {case}");
                }
                for s in u.scopes() {
                    if !matches!(s.kind, ScopeKind::Unknown(_)) {
                        assert!(s.path_len() > 0, "case {case}");
                        assert!(s.path().all(|(dv, f)| dv < 32 && f < 8), "case {case}");
                    }
                    let _ = s.resolve(u.segment, &mut model);
                }
            }
            for r in d.reserved_memory() {
                assert!(
                    r.base.is_multiple_of(4096) && r.limit >= r.base,
                    "case {case}"
                );
                assert_eq!(r.limit & 0xFFF, 0xFFF, "case {case}");
            }
            for r in [at(0, 2, 0), at(0, 0x14, 0), at(2, 3, 1), at(0, 0x1C, 0)] {
                let _ = d.unit_for(r, &mut model);
            }
            // An I/O APIC that resolves is listed, exactly once, by the unit returned.
            for id in [0, 2, 0xFF] {
                if let Ok((u, r)) = d.ioapic(id, &mut model) {
                    let listing = d
                        .units()
                        .flat_map(|u| u.scopes())
                        .filter(|s| s.kind == ScopeKind::IoApic && s.enumeration_id == id)
                        .count();
                    assert_eq!(listing, 1, "case {case}");
                    assert!(
                        u.scopes().any(|s| s.kind == ScopeKind::IoApic
                            && s.enumeration_id == id
                            && s.resolve(u.segment, &mut model) == Some(r)),
                        "case {case}"
                    );
                }
            }
            let _ = d.structures().count();
        }
        assert!(accepted > 1000, "{accepted} accepted");
    }
}
