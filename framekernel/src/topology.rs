//! CPU topology from CPUID (CLAUDE.md §24 CPU topology, SMT awareness and hybrid-core
//! capabilities, §27 topology-aware scheduling; research ledger
//! `docs/research/2026-11-08-cpu-topology-leaf-1fh.md`).
//!
//! An x2APIC ID is a bit field: from the bottom, the ID of a thread within its core, a core
//! within its module, a module within its tile, a tile within its die, a die within its die
//! group, a die group within its package, and the package above them all. Each boundary is
//! a power of two: firmware rounds every field up, so IDs are sparse. This module finds
//! the boundaries from the CPUID leaf a processor reports them in, splits an ID into
//! domain IDs, and compares what the processors say about themselves with the ACPI MADT.
//! Pure, no `unsafe`, tested on the host; the kernel runs CPUID and passes the reader in.
//!
//! Three leaves enumerate a layout, one sub-leaf per level, narrowest first:
//! - **Leaf 1Fh** (V2 Extended Topology; Intel SDM Vol. 1, Chapter 21, 253665-093) and
//!   **leaf 0Bh** (Extended Topology): EAX\[4:0\] is the shift to the *next wider* domain.
//!   ECX\[15:8\] names the domain: 1 logical processor, 2 core, 3 module, 4 tile, 5 die,
//!   6 die group, the package being implied above the last. Sub-leaves end where EBX\[15:0\]
//!   is 0. A domain type not known here is absorbed into the known domain directly below it
//!   (Intel, *Processor Topology Enumeration*, 337015-002, §1.3.2): ignoring it would show
//!   too many packages.
//! - **Leaf 80000026h** (Extended CPU Topology; AMD APM Vol. 3, 24594 rev 3.39): EAX\[4:0\]
//!   is the shift to *this* level's own ID, and ECX\[15:8\] is 1 core, 2 complex, 3 die,
//!   4 socket. It ends at level type 0. The same numbers therefore mean something else than
//!   in 1Fh, which is why the two are decoded apart and normalized into one [`Layout`].
//!
//! A processor reports only on itself: on a hybrid part, leaves can differ between logical
//! processors, and AMD says so in as many words. So each processor reads its own record
//! ([`CpuTopology::detect`]) on itself.
//!
//! A leaf above the maximum a processor reports is not zero: Intel documents, and QEMU 11.1
//! does, that it returns the highest basic leaf's data. On QEMU's Intel-vendor model, 80000026h
//! then reads as a valid-looking 1Fh list. Every leaf here is read only below its maximum.
//!
//! Core types (leaf 1Ah; AMD's EBX\[31:16\]) are recorded as raw codes. Intel's text says a
//! core type's numeric value "has no significance, neither large nor small", and that it
//! implies no other attribute: capacity must come from elsewhere (§24: capabilities, never
//! marketing names).

use core::fmt;

/// Domains in a [`Layout`].
const DOMAINS: usize = 7;

/// Sub-leaves read from one topology leaf before the list is declared unterminated. Intel's
/// paper shows seven levels; this is ample room with the terminator.
pub const MAX_LEVELS: u32 = 16;

/// Leaf 0's sub-leaf list is empty, in EBX\[15:0\].
const NO_DOMAIN: u32 = 0;

/// Intel's domain type codes (ECX\[15:8\] of leaves 1Fh and 0Bh).
const fn domain_type(ecx: u32) -> u32 {
    (ecx >> 8) & 0xFF
}

/// A scope of the processor hierarchy, narrowest first: each domain's instances group
/// instances of the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Domain {
    /// A logical processor: an SMT thread.
    Thread,
    /// A core.
    Core,
    /// A group of cores that share resources: a module, or on AMD a core complex.
    Module,
    /// A tile.
    Tile,
    /// A die.
    Die,
    /// A group of dies that share resources.
    DieGroup,
    /// A package (socket): implied above the last enumerated level by leaves 1Fh and 0Bh.
    Package,
}

impl Domain {
    /// Every domain, narrowest first.
    pub const ALL: [Domain; DOMAINS] = [
        Domain::Thread,
        Domain::Core,
        Domain::Module,
        Domain::Tile,
        Domain::Die,
        Domain::DieGroup,
        Domain::Package,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    /// The domain a leaf 1Fh or 0Bh domain type code names, if it is known here.
    const fn from_shifted_type(code: u32) -> Option<Domain> {
        match code {
            1 => Some(Domain::Thread),
            2 => Some(Domain::Core),
            3 => Some(Domain::Module),
            4 => Some(Domain::Tile),
            5 => Some(Domain::Die),
            6 => Some(Domain::DieGroup),
            _ => None,
        }
    }

    /// The domain a leaf 80000026h level type names, if it is known here.
    const fn from_level_type(code: u32) -> Option<Domain> {
        match code {
            1 => Some(Domain::Core),
            2 => Some(Domain::Module),
            3 => Some(Domain::Die),
            4 => Some(Domain::Package),
            _ => None,
        }
    }
}

/// How an x2APIC ID splits into domain IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// For each domain, in [`Domain`] order, the low bits of an ID below its ID field. Each
    /// is at most 31: they come from 5-bit CPUID fields.
    below: [u8; DOMAINS],
}

impl Layout {
    /// No leaf says otherwise: every bit of an ID is the package.
    pub const FLAT: Layout = Layout {
        below: [0; DOMAINS],
    };

    /// The bits of an ID below `domain`'s ID: shifting an ID right by this leaves the
    /// system-wide ID of that domain instance.
    pub const fn shift(&self, domain: Domain) -> u32 {
        self.below[domain.index()] as u32
    }

    /// The width of `domain`'s ID field, within its parent. The package takes what remains
    /// of the 32 bits. A domain the processor does not enumerate has width 0.
    pub const fn width(&self, domain: Domain) -> u32 {
        match domain {
            Domain::Package => 32 - self.shift(Domain::Package),
            _ => self.below[domain.index() + 1] as u32 - self.shift(domain),
        }
    }

    /// `domain`'s ID within its parent, for the processor whose x2APIC ID is `apic_id`.
    pub const fn id(&self, domain: Domain, apic_id: u32) -> u32 {
        let id = apic_id >> self.shift(domain);
        match domain {
            Domain::Package => id,
            _ => id & ((1u32 << self.width(domain)) - 1),
        }
    }

    /// Where `apic_id` sits in this layout.
    pub const fn place(&self, apic_id: u32) -> Placement {
        Placement {
            package: self.id(Domain::Package, apic_id),
            die_group: match self.width(Domain::DieGroup) {
                0 => None,
                _ => Some(self.id(Domain::DieGroup, apic_id)),
            },
            die: self.id(Domain::Die, apic_id),
            tile: match self.width(Domain::Tile) {
                0 => None,
                _ => Some(self.id(Domain::Tile, apic_id)),
            },
            module: self.id(Domain::Module, apic_id),
            core: self.id(Domain::Core, apic_id),
            thread: self.id(Domain::Thread, apic_id),
        }
    }

    /// Threads a core may have.
    pub const fn threads_per_core(&self) -> u32 {
        1 << self.width(Domain::Thread)
    }

    /// Logical processors a package may have.
    pub const fn logical_per_package(&self) -> u32 {
        1 << self.shift(Domain::Package)
    }

    /// Whether two processors are threads of one core: their IDs differ only in the thread
    /// field. On a mixed part, use the wider of the two processors' layouts.
    pub const fn are_siblings(&self, a: u32, b: u32) -> bool {
        a >> self.shift(Domain::Core) == b >> self.shift(Domain::Core)
    }

    /// Counts the domain instances among `ids`, which it sorts. An ID given twice is counted
    /// once. The instance of a domain is the ID shifted past that domain's field, so
    /// equal instances sit next to each other once the IDs are sorted.
    pub fn census(&self, ids: &mut [u32]) -> Census {
        ids.sort_unstable();
        let count = |domain: Domain| {
            let shift = self.shift(domain);
            let distinct = ids
                .windows(2)
                .filter(|w| w[0] >> shift != w[1] >> shift)
                .count();
            if ids.is_empty() { 0 } else { distinct + 1 }
        };
        Census {
            packages: count(Domain::Package),
            die_groups: count(Domain::DieGroup),
            dies: count(Domain::Die),
            tiles: count(Domain::Tile),
            modules: count(Domain::Module),
            cores: count(Domain::Core),
            threads: count(Domain::Thread),
        }
    }

    /// Whether `other` draws the same boundaries above the thread field: from the top of the
    /// core field up, so a core's boundary with the thread field may differ, as between a
    /// core with SMT and one without (the SDM allows either 0 or 1 bit there for a core
    /// without SMT).
    const fn same_above_thread(&self, other: &Layout) -> bool {
        let mut i = Domain::Module.index();
        while i < DOMAINS {
            if self.below[i] != other.below[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The layout whose fields end at `through` (each known domain's upper boundary, narrowest
    /// first; `None` for a domain the leaf does not enumerate). What is below the first field
    /// that is present is empty.
    fn from_upper_bounds(through: [Option<u8>; DOMAINS]) -> Layout {
        let mut below = [0; DOMAINS];
        let mut lower = 0;
        for domain in Domain::ALL {
            below[domain.index()] = lower;
            if let Some(end) = through[domain.index()] {
                lower = end;
            }
        }
        Layout { below }
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "thread bits {}, core bits {}, module bits {}",
            self.width(Domain::Thread),
            self.width(Domain::Core),
            self.width(Domain::Module)
        )?;
        if self.width(Domain::Tile) != 0 {
            write!(f, ", tile bits {}", self.width(Domain::Tile))?;
        }
        write!(f, ", die bits {}", self.width(Domain::Die))?;
        if self.width(Domain::DieGroup) != 0 {
            write!(f, ", die group bits {}", self.width(Domain::DieGroup))?;
        }
        write!(f, ", package from bit {}", self.shift(Domain::Package))
    }
}

/// Where a processor sits: each domain's ID within its parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Its package.
    pub package: u32,
    /// Its die group, when the layout has any.
    pub die_group: Option<u32>,
    /// Its die within the die group or package.
    pub die: u32,
    /// Its tile within the die, when the layout has any.
    pub tile: Option<u32>,
    /// Its module (or AMD core complex) within the die or tile.
    pub module: u32,
    /// Its core within the module.
    pub core: u32,
    /// Its thread within the core.
    pub thread: u32,
}

impl fmt::Display for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "package {}", self.package)?;
        if let Some(group) = self.die_group {
            write!(f, ", die group {group}")?;
        }
        write!(f, ", die {}", self.die)?;
        if let Some(tile) = self.tile {
            write!(f, ", tile {tile}")?;
        }
        write!(
            f,
            ", module {}, core {}, thread {}",
            self.module, self.core, self.thread
        )
    }
}

/// How many instances of each domain a set of processors spans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Census {
    /// Packages.
    pub packages: usize,
    /// Die groups.
    pub die_groups: usize,
    /// Dies.
    pub dies: usize,
    /// Tiles.
    pub tiles: usize,
    /// Modules.
    pub modules: usize,
    /// Cores.
    pub cores: usize,
    /// Threads: the processors themselves.
    pub threads: usize,
}

/// The CPUID leaf a layout came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Leaf 1Fh, V2 Extended Topology.
    Leaf1F,
    /// Leaf 0Bh, Extended Topology.
    Leaf0B,
    /// Leaf 80000026h, Extended CPU Topology.
    Leaf80000026,
    /// None: the processor implements no leaf used here.
    None,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Leaf1F => "leaf 1Fh",
            Source::Leaf0B => "leaf 0Bh",
            Source::Leaf80000026 => "leaf 80000026h",
            Source::None => "no topology leaf",
        })
    }
}

/// Why a leaf that claims to be implemented could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// Sub-leaf 0 of leaf 1Fh or 0Bh does not describe the logical-processor domain.
    NoThreadLevel,
    /// No terminating sub-leaf within [`MAX_LEVELS`].
    Unterminated,
    /// A known domain came after one at least as wide.
    OutOfOrder,
    /// A shift is smaller than the one before it.
    ShiftsDecrease,
    /// A sub-leaf has a non-zero count and domain type 0, which the SDM rules out.
    InvalidDomain,
    /// EDX, the processor's own ID, changed between sub-leaves.
    ApicIdVaries,
    /// Leaf 80000026h lists no core or no socket level, so a thread or package boundary is
    /// unknown.
    MissingLevel,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LayoutError::NoThreadLevel => "sub-leaf 0 is not the logical-processor domain",
            LayoutError::Unterminated => "no terminating sub-leaf",
            LayoutError::OutOfOrder => "domains out of order",
            LayoutError::ShiftsDecrease => "shifts decrease",
            LayoutError::InvalidDomain => "domain type 0 with a non-zero count",
            LayoutError::ApicIdVaries => "the x2APIC ID differs between sub-leaves",
            LayoutError::MissingLevel => "no core or no socket level",
        })
    }
}

/// What a processor says about its own core. On a hybrid part it may distinguish cores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreIdentity {
    /// A code naming the core's microarchitecture, as the processor reports it. It has no
    /// order, and implies no performance or power property.
    pub core_type: u8,
    /// A further code distinguishing implementations of that type.
    pub native_model: u32,
    /// A static efficiency ranking among cores of a type, when the processor reports one:
    /// a lower value means lower power and lower performance (leaf 80000026h, AMD).
    pub efficiency_rank: Option<u8>,
}

/// What one processor says about itself, read on that processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTopology {
    /// The leaf its layout came from.
    pub source: Source,
    /// Its layout.
    pub layout: Layout,
    /// Its x2APIC ID, from the same leaf (leaf 1 EBX\[31:24\] without one).
    pub apic_id: u32,
    /// Whether the processor reports a mix of core types in the system: CPUID.07H:EDX\[15\],
    /// or HeterogeneousCores in leaf 80000026h.
    pub hybrid: bool,
    /// Its core's identity, where the processor reports one.
    pub core: Option<CoreIdentity>,
    /// A leaf it claims to implement that could not be decoded, the fallback taken instead.
    pub problem: Option<LayoutError>,
}

/// What leaf 80000026h adds to a layout and the processor's ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExtendedDetails {
    core: CoreIdentity,
    heterogeneous: bool,
}

/// Decodes a leaf whose shifts lead to the next wider domain: 1Fh or 0Bh. `read(n)` returns
/// sub-leaf `n` of it, `[eax, ebx, ecx, edx]`.
fn decode_next_level(read: impl Fn(u32) -> [u32; 4]) -> Result<(Layout, u32), LayoutError> {
    let mut through = [None; DOMAINS];
    let mut current: Option<Domain> = None;
    let mut widest = 0;
    let mut apic_id = 0;
    for sub in 0..MAX_LEVELS {
        let [eax, ebx, ecx, edx] = read(sub);
        if ebx & 0xFFFF == NO_DOMAIN {
            return if current.is_some() {
                Ok((Layout::from_upper_bounds(through), apic_id))
            } else {
                Err(LayoutError::NoThreadLevel)
            };
        }
        let code = domain_type(ecx);
        if code == NO_DOMAIN {
            return Err(LayoutError::InvalidDomain);
        }
        let shift = (eax & 0x1F) as u8;
        if shift < widest {
            return Err(LayoutError::ShiftsDecrease);
        }
        widest = shift;
        if sub == 0 {
            apic_id = edx;
        } else if edx != apic_id {
            return Err(LayoutError::ApicIdVaries);
        }
        match Domain::from_shifted_type(code) {
            Some(domain) => {
                if sub == 0 && domain != Domain::Thread {
                    return Err(LayoutError::NoThreadLevel);
                }
                if current.is_some_and(|c| domain <= c) {
                    return Err(LayoutError::OutOfOrder);
                }
                through[domain.index()] = Some(shift);
                current = Some(domain);
            }
            // An unknown domain: its bits go to the known domain directly below it.
            None => match current {
                Some(domain) => through[domain.index()] = Some(shift),
                None => return Err(LayoutError::NoThreadLevel),
            },
        }
    }
    Err(LayoutError::Unterminated)
}

/// Decodes leaf 80000026h, whose shifts lead to each level's own ID. `read(n)` returns
/// sub-leaf `n` of it, `[eax, ebx, ecx, edx]`.
fn decode_own_level(
    read: impl Fn(u32) -> [u32; 4],
) -> Result<(Layout, u32, ExtendedDetails), LayoutError> {
    let mut starts = [None; DOMAINS];
    let mut last: Option<Domain> = None;
    let mut widest = 0;
    let mut apic_id = 0;
    let mut core_words = None;
    let mut heterogeneous = false;
    for sub in 0..MAX_LEVELS {
        let [eax, ebx, ecx, edx] = read(sub);
        let code = domain_type(ecx);
        if code == NO_DOMAIN {
            return match (core_words, starts[Domain::Package.index()]) {
                (Some(words), Some(package)) => {
                    let core = identity_from_core_level(words);
                    Ok((
                        layout_from_lower_bounds(package, starts),
                        apic_id,
                        ExtendedDetails {
                            core,
                            heterogeneous,
                        },
                    ))
                }
                _ => Err(LayoutError::MissingLevel),
            };
        }
        let shift = (eax & 0x1F) as u8;
        if shift < widest {
            return Err(LayoutError::ShiftsDecrease);
        }
        widest = shift;
        if sub == 0 {
            apic_id = edx;
        } else if edx != apic_id {
            return Err(LayoutError::ApicIdVaries);
        }
        heterogeneous |= eax & (1 << 30) != 0;
        if let Some(domain) = Domain::from_level_type(code) {
            if last.is_some_and(|l| domain <= l) {
                return Err(LayoutError::OutOfOrder);
            }
            last = Some(domain);
            starts[domain.index()] = Some(shift);
            if domain == Domain::Core {
                core_words = Some((eax, ebx));
            }
        }
        // A reserved level type is ignored: the known level below keeps its bits, since only
        // the next known level bounds it.
    }
    Err(LayoutError::Unterminated)
}

/// The layout whose fields start at `starts` (`package` being the socket's). A level leaf
/// 80000026h does not list has an empty field: it starts where the next listed level does.
/// The thread field is everything below the core field.
fn layout_from_lower_bounds(package: u8, starts: [Option<u8>; DOMAINS]) -> Layout {
    let mut below = [0; DOMAINS];
    below[Domain::Package.index()] = package;
    let mut upper = package;
    for domain in [
        Domain::DieGroup,
        Domain::Die,
        Domain::Tile,
        Domain::Module,
        Domain::Core,
    ] {
        upper = starts[domain.index()].unwrap_or(upper);
        below[domain.index()] = upper;
    }
    Layout { below }
}

/// The core identity leaf 80000026h gives at its core level: `(eax, ebx)` of that sub-leaf.
const fn identity_from_core_level((eax, ebx): (u32, u32)) -> CoreIdentity {
    CoreIdentity {
        core_type: (ebx >> 28) as u8,
        native_model: (ebx >> 24) & 0xF,
        efficiency_rank: if eax & (1 << 29) != 0 {
            Some(((ebx >> 16) & 0xFF) as u8)
        } else {
            None
        },
    }
}

/// CPUID.07H:EDX\[15\]: the processor is a hybrid part.
const HYBRID: u32 = 1 << 15;

impl CpuTopology {
    /// Reads this processor's topology: `cpuid(leaf, sub)` must return that CPUID leaf and
    /// sub-leaf, `[eax, ebx, ecx, edx]`, run on this very processor.
    ///
    /// Leaf 80000026h is taken first where the processor has it, then 1Fh, then 0Bh; Intel
    /// and AMD each recommend their own, and this is the one where each is the richer.
    /// Every leaf is checked against its maximum first (the module comment says why). A leaf
    /// that claims to be there but cannot be decoded is skipped for the next one and
    /// recorded in `problem`.
    pub fn detect(cpuid: impl Fn(u32, u32) -> [u32; 4]) -> CpuTopology {
        let max_basic = cpuid(0, 0)[0];
        let max_extended = cpuid(0x8000_0000, 0)[0];
        let mut problem = None;
        let mut core = None;
        let mut hybrid = max_basic >= 7 && cpuid(7, 0)[3] & HYBRID != 0;

        let mut found = None;
        if max_extended >= 0x8000_0026 && domain_type(cpuid(0x8000_0026, 0)[2]) != NO_DOMAIN {
            match decode_own_level(|sub| cpuid(0x8000_0026, sub)) {
                Ok((layout, apic_id, details)) => {
                    found = Some((Source::Leaf80000026, layout, apic_id));
                    core = Some(details.core);
                    hybrid |= details.heterogeneous;
                }
                Err(e) => problem = Some(e),
            }
        }
        for (leaf, source) in [(0x1F, Source::Leaf1F), (0x0B, Source::Leaf0B)] {
            if found.is_some() || max_basic < leaf || cpuid(leaf, 0)[1] & 0xFFFF == NO_DOMAIN {
                continue;
            }
            match decode_next_level(|sub| cpuid(leaf, sub)) {
                Ok((layout, apic_id)) => found = Some((source, layout, apic_id)),
                Err(e) => problem = Some(e),
            }
        }
        let (source, layout, apic_id) =
            found.unwrap_or((Source::None, Layout::FLAT, cpuid(1, 0)[1] >> 24));

        if core.is_none() && max_basic >= 0x1A {
            let eax = cpuid(0x1A, 0)[0];
            if eax != 0 {
                core = Some(CoreIdentity {
                    core_type: (eax >> 24) as u8,
                    native_model: eax & 0x00FF_FFFF,
                    efficiency_rank: None,
                });
            }
        }
        CpuTopology {
            source,
            layout,
            apic_id,
            hybrid,
            core,
            problem,
        }
    }

    /// Where this processor sits, by its own layout and ID.
    pub const fn place(&self) -> Placement {
        self.layout.place(self.apic_id)
    }
}

/// How the online processors agree with the ACPI MADT and with each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Agreement {
    /// Processors with a record.
    pub checked: usize,
    /// Processors that never recorded themselves.
    pub missing: usize,
    /// Processors whose own CPUID x2APIC ID is not the one the MADT lists for them.
    pub wrong_apic_id: usize,
    /// Processors whose layout differs from the first's.
    pub layout_differs: usize,
    /// Processors with a leaf they claim to implement that could not be decoded.
    pub undecodable: usize,
    /// Hybrid processors that report no core type.
    pub unidentified: usize,
    /// Processors with the same placement as an earlier one.
    pub duplicates: usize,
}

impl Agreement {
    /// Whether nothing disagrees.
    pub const fn holds(&self) -> bool {
        self.missing == 0
            && self.wrong_apic_id == 0
            && self.layout_differs == 0
            && self.undecodable == 0
            && self.unidentified == 0
            && self.duplicates == 0
    }
}

/// Compares the online processors, the boot processor first: each pair is the x2APIC ID the
/// MADT lists for a processor and what it recorded about itself.
///
/// Layouts must be the same from the core domain up. The thread field may differ only on a
/// hybrid processor (the SDM allows 0 or 1 bit there on a core without SMT).
pub fn agree(cpus: &[(u32, Option<CpuTopology>)]) -> Agreement {
    let mut result = Agreement::default();
    for (i, (madt_id, own)) in cpus.iter().enumerate() {
        let Some(own) = own else {
            result.missing += 1;
            continue;
        };
        result.checked += 1;
        if own.apic_id != *madt_id {
            result.wrong_apic_id += 1;
        }
        if own.problem.is_some() {
            result.undecodable += 1;
        }
        if own.hybrid && own.core.is_none() {
            result.unidentified += 1;
        }
        if let Some(first) = cpus.iter().filter_map(|c| c.1.as_ref()).next() {
            let same_threads = first.layout.width(Domain::Thread)
                == own.layout.width(Domain::Thread)
                || first.hybrid
                || own.hybrid;
            if first.source != own.source
                || !first.layout.same_above_thread(&own.layout)
                || !same_threads
            {
                result.layout_differs += 1;
            }
        }
        if cpus[..i]
            .iter()
            .filter_map(|c| c.1.as_ref())
            .any(|other| other.place() == own.place())
        {
            result.duplicates += 1;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rng, cases};
    use std::string::ToString;

    /// One processor's CPUID: its leaves and sub-leaves, and what a read above the maximum
    /// returns (the highest basic leaf's data, as QEMU 11.1 does and the SDM documents).
    struct FakeCpu {
        max_basic: u32,
        max_extended: u32,
        leaves: Vec<((u32, u32), [u32; 4])>,
    }

    impl FakeCpu {
        fn new(max_basic: u32, max_extended: u32) -> Self {
            Self {
                max_basic,
                max_extended,
                leaves: Vec::new(),
            }
        }

        fn with(mut self, leaf: u32, subleaves: &[[u32; 4]]) -> Self {
            for (sub, words) in subleaves.iter().enumerate() {
                self.leaves.push(((leaf, sub as u32), *words));
            }
            self
        }

        fn cpuid(&self, leaf: u32, sub: u32) -> [u32; 4] {
            if leaf == 0 {
                return [self.max_basic, 0, 0, 0];
            }
            if leaf == 0x8000_0000 {
                return [self.max_extended, 0, 0, 0];
            }
            let limit = if leaf >= 0x8000_0000 {
                self.max_extended
            } else {
                self.max_basic
            };
            let leaf = if leaf > limit { self.max_basic } else { leaf };
            self.leaves
                .iter()
                .find(|&&((l, s), _)| l == leaf && s == sub)
                .map_or([0; 4], |&(_, words)| words)
        }

        fn detect(&self) -> CpuTopology {
            CpuTopology::detect(|leaf, sub| self.cpuid(leaf, sub))
        }
    }

    /// QEMU 11.1.3's leaf 1Fh for `sockets=2,dies=3,modules=5,cores=5,threads=1`, measured on
    /// an Intel-vendor model (`id` is the processor's x2APIC ID).
    fn qemu_1f(id: u32) -> [[u32; 4]; 5] {
        [
            [0, 1, 0x100, id],
            [3, 5, 0x201, id],
            [6, 0x19, 0x302, id],
            [8, 0x4B, 0x503, id],
            [0, 0, 4, id],
        ]
    }

    /// QEMU 11.1.3's leaf 0Bh for that machine.
    fn qemu_0b(id: u32) -> [[u32; 4]; 3] {
        [[0, 1, 0x100, id], [8, 0x4B, 0x201, id], [0, 0, 2, id]]
    }

    /// The x2APIC IDs of the 150 possible processors of that machine.
    fn sparse_ids() -> Vec<u32> {
        let mut out = Vec::new();
        for package in 0..2u32 {
            for die in 0..3u32 {
                for module in 0..5u32 {
                    for core in 0..5u32 {
                        out.push(package << 8 | die << 6 | module << 3 | core);
                    }
                }
            }
        }
        out
    }

    /// Words of a leaf 1Fh-shaped list: `(domain type, shift)` per sub-leaf, for processor
    /// `id`, with a terminator.
    fn shifted(id: u32, levels: &[(u32, u32)]) -> Vec<[u32; 4]> {
        let mut out = Vec::new();
        for (sub, &(code, shift)) in levels.iter().enumerate() {
            out.push([shift, 1, (code << 8) | sub as u32, id]);
        }
        out.push([0, 0, levels.len() as u32, id]);
        out
    }

    #[test]
    fn leaf_1f_splits_qemus_sparse_machine() {
        let cpu = FakeCpu::new(0x1F, 0x8000_0021)
            .with(0x1F, &qemu_1f(412))
            .with(0x0B, &qemu_0b(412));
        let own = cpu.detect();
        assert_eq!(own.source, Source::Leaf1F);
        assert_eq!(own.problem, None);
        assert_eq!(own.apic_id, 412);
        assert_eq!(
            own.layout.to_string(),
            "thread bits 0, core bits 3, module bits 3, die bits 2, package from bit 8"
        );
        assert_eq!(
            own.place().to_string(),
            "package 1, die 2, module 3, core 4, thread 0"
        );
        let mut ids = sparse_ids();
        assert_eq!(
            own.layout.census(&mut ids),
            Census {
                packages: 2,
                die_groups: 2,
                dies: 6,
                tiles: 6,
                modules: 30,
                cores: 150,
                threads: 150,
            }
        );
    }

    #[test]
    fn leaf_0b_sees_no_die_or_module_on_the_same_machine() {
        // The leaf a processor without 1Fh falls back on: the core field takes every bit up
        // to the package, as QEMU's -cpu max (AMD vendor, level 0Dh) reports it.
        let cpu = FakeCpu::new(0x0D, 0x8000_0021).with(0x0B, &qemu_0b(412));
        let own = cpu.detect();
        assert_eq!(own.source, Source::Leaf0B);
        assert_eq!(
            own.layout.to_string(),
            "thread bits 0, core bits 8, module bits 0, die bits 0, package from bit 8"
        );
        assert_eq!(
            own.place().to_string(),
            "package 1, die 0, module 0, core 156, thread 0"
        );
        let mut ids = sparse_ids();
        let census = own.layout.census(&mut ids);
        assert_eq!((census.packages, census.dies), (2, 2));
        assert_eq!(
            (census.modules, census.cores, census.threads),
            (2, 150, 150)
        );
    }

    #[test]
    fn a_leaf_above_the_maximum_is_not_believed() {
        // What -cpu max under TCG actually returns for 1Fh and 80000026h with level 0Dh and
        // xlevel 80000021h: the data of leaf 0Dh (the XSAVE sub-leaves), which would decode
        // to rubbish. Only 0Bh is below the maximum.
        let xsave = [
            [0x21F, 0x340, 0xA88, 0],
            [5, 0x3C8, 0, 0],
            [0x100, 0x240, 0, 0],
            [0x40, 0x3C0, 0, 0],
        ];
        let cpu = FakeCpu::new(0x0D, 0x8000_0021)
            .with(0x0D, &xsave)
            .with(0x0B, &qemu_0b(1));
        let own = cpu.detect();
        assert_eq!(own.source, Source::Leaf0B);
        assert_eq!(own.problem, None);
        assert_eq!(own.core, None);
        assert!(!own.hybrid);

        // An Intel-vendor model: level 1Fh, xlevel 80000021h. 80000026h above that maximum
        // returns 1Fh's list, a valid-looking list with the other meaning of the shifts.
        let cpu = FakeCpu::new(0x1F, 0x8000_0021)
            .with(0x1F, &qemu_1f(256))
            .with(0x0B, &qemu_0b(256));
        let own = cpu.detect();
        assert_eq!(own.source, Source::Leaf1F);
        assert_eq!(own.layout.shift(Domain::Package), 8);
    }

    #[test]
    fn with_no_leaf_each_processor_is_its_own_package() {
        // Leaf 1's EBX[31:24] is the 8-bit initial APIC ID.
        let cpu = FakeCpu::new(5, 0x8000_0008).with(1, &[[0, 7 << 24, 0, 0]]);
        let own = cpu.detect();
        assert_eq!(own.source, Source::None);
        assert_eq!(own.layout, Layout::FLAT);
        assert_eq!(own.apic_id, 7);
        assert_eq!(own.layout.threads_per_core(), 1);
        assert_eq!(
            own.place().to_string(),
            "package 7, die 0, module 0, core 0, thread 0"
        );
        assert!(!own.layout.are_siblings(0, 1));
    }

    #[test]
    fn leaf_1f_with_every_domain() {
        // thread [0], core [3:1], module [5:4], tile [6], die [8:7], die group [9].
        let levels = [(1, 1), (2, 4), (3, 6), (4, 7), (5, 9), (6, 10)];
        let cpu = FakeCpu::new(0x1F, 0).with(
            0x1F,
            &shifted(1 << 10 | 2 << 7 | 1 << 6 | 2 << 4 | 5 << 1, &levels),
        );
        let own = cpu.detect();
        assert_eq!(own.source, Source::Leaf1F);
        assert_eq!(
            own.layout.to_string(),
            "thread bits 1, core bits 3, module bits 2, tile bits 1, die bits 2, die group bits 1, package from bit 10"
        );
        assert_eq!(
            own.place().to_string(),
            "package 1, die group 0, die 2, tile 1, module 2, core 5, thread 0"
        );
        assert_eq!(own.layout.threads_per_core(), 2);
        assert_eq!(own.layout.logical_per_package(), 1 << 10);
        assert!(own.layout.are_siblings(0b1000, 0b1001));
        assert!(!own.layout.are_siblings(0b1000, 0b1010));
    }

    #[test]
    fn unknown_domains_are_absorbed_into_the_known_one_below() {
        // Intel's paper (337015-002), Example B: core [2:1], unknown [3] → core [3:1].
        let cpu = FakeCpu::new(0x1F, 0).with(0x1F, &shifted(0, &[(1, 1), (2, 3), (9, 4)]));
        let layout = cpu.detect().layout;
        assert_eq!(layout.width(Domain::Core), 3);
        assert_eq!(layout.shift(Domain::Package), 4);

        // Example C: core [2:1], unknown [3], die [5:4] → core [3:1], die [5:4].
        let cpu = FakeCpu::new(0x1F, 0).with(0x1F, &shifted(0, &[(1, 1), (2, 3), (7, 4), (5, 6)]));
        let layout = cpu.detect().layout;
        assert_eq!(layout.shift(Domain::Core), 1);
        assert_eq!(layout.width(Domain::Core), 3);
        assert_eq!(layout.width(Domain::Die), 2);
        assert_eq!(layout.shift(Domain::Package), 6);

        // Example D: core [2:1], unknown [3], module [5:4], die [7:6], unknown [8],
        // unknown [10:9] → core [3:1], module [5:4], die [10:6].
        let levels = [(1, 1), (2, 3), (8, 4), (3, 6), (5, 8), (9, 9), (0xFE, 11)];
        let cpu = FakeCpu::new(0x1F, 0).with(0x1F, &shifted(0, &levels));
        let layout = cpu.detect().layout;
        assert_eq!(layout.width(Domain::Core), 3);
        assert_eq!(layout.width(Domain::Module), 2);
        assert_eq!(layout.width(Domain::Die), 5);
        assert_eq!(layout.shift(Domain::Package), 11);
    }

    #[test]
    fn a_core_level_of_shift_zero_means_one_core() {
        // N = 1 under QEMU: one thread per core, one core per package.
        let cpu = FakeCpu::new(0x0D, 0).with(0x0B, &[[0, 1, 0x100, 0], [0, 1, 0x201, 0], [0; 4]]);
        let own = cpu.detect();
        assert_eq!(own.layout.width(Domain::Thread), 0);
        assert_eq!(own.layout.width(Domain::Core), 0);
        assert_eq!(own.layout.shift(Domain::Package), 0);
        assert_eq!(own.layout.logical_per_package(), 1);
    }

    /// A malformed leaf 1Fh, with a good 0Bh to fall back on.
    fn fallback_after(subleaves: &[[u32; 4]]) -> CpuTopology {
        FakeCpu::new(0x1F, 0)
            .with(0x1F, subleaves)
            .with(0x0B, &qemu_0b(9))
            .detect()
    }

    #[test]
    fn a_malformed_leaf_is_skipped_and_reported() {
        let cases: [(&[[u32; 4]], LayoutError); 6] = [
            // Sub-leaf 0 is a core, not a logical processor.
            (&[[1, 2, 0x200, 9], [0; 4]], LayoutError::NoThreadLevel),
            // Shifts that decrease.
            (
                &[[3, 1, 0x100, 9], [2, 4, 0x201, 9], [0; 4]],
                LayoutError::ShiftsDecrease,
            ),
            // A module above a die.
            (
                &[[0, 1, 0x100, 9], [3, 3, 0x501, 9], [4, 6, 0x302, 9], [0; 4]],
                LayoutError::OutOfOrder,
            ),
            // The same domain twice.
            (
                &[[0, 1, 0x100, 9], [1, 3, 0x201, 9], [2, 6, 0x202, 9], [0; 4]],
                LayoutError::OutOfOrder,
            ),
            // Domain type 0 with a count.
            (
                &[[0, 1, 0x100, 9], [1, 3, 0x001, 9], [0; 4]],
                LayoutError::InvalidDomain,
            ),
            // The x2APIC ID changes between sub-leaves.
            (
                &[[0, 1, 0x100, 9], [3, 3, 0x201, 8], [0; 4]],
                LayoutError::ApicIdVaries,
            ),
        ];
        for (subleaves, error) in cases {
            let own = fallback_after(subleaves);
            assert_eq!(own.problem, Some(error), "{subleaves:x?}");
            assert_eq!(own.source, Source::Leaf0B, "{subleaves:x?}");
            assert_eq!(own.apic_id, 9);
        }
        // No terminator within the bound.
        let unending: Vec<[u32; 4]> = (0..MAX_LEVELS)
            .map(|sub| [sub + 1, 1, (0xE0 << 8) | sub, 9])
            .collect();
        let mut list = vec![[0, 1, 0x100, 9]];
        list.extend(unending);
        let own = fallback_after(&list);
        assert_eq!(own.problem, Some(LayoutError::Unterminated));
    }

    #[test]
    fn a_leaf_with_a_zero_count_is_absent_not_malformed() {
        // Vol. 1: the leaf is valid only if EBX[15:0] of sub-leaf 0 is non-zero. QEMU's
        // default machine has this shape for 1Fh on an Intel-vendor model with min-level 1Fh.
        let own = FakeCpu::new(0x1F, 0)
            .with(0x1F, &[[0, 0, 0, 0]])
            .with(0x0B, &qemu_0b(2))
            .detect();
        assert_eq!(own.source, Source::Leaf0B);
        assert_eq!(own.problem, None);
    }

    /// Leaf 80000026h words for processor `id`: `(level type, mask width, eax flags, ebx
    /// bits 31:16)` per sub-leaf, with a terminator.
    fn extended(id: u32, levels: &[(u32, u32, u32, u32)]) -> Vec<[u32; 4]> {
        let mut out = Vec::new();
        for (sub, &(kind, width, flags, top)) in levels.iter().enumerate() {
            out.push([width | flags, 1 | top << 16, (kind << 8) | sub as u32, id]);
        }
        out.push([0, 0, levels.len() as u32, id]);
        out
    }

    #[test]
    fn leaf_80000026_shifts_lead_to_each_levels_own_id() {
        // SMT: 1 bit; cores [3:1], complex [4], die [5], socket from bit 6: core level 1,
        // complex 2, die 3, socket 4 in the sub-leaves, and each width gives that level's ID
        // (unlike 1Fh, where it would give the next one's).
        let levels = [(1, 1, 0, 0), (2, 4, 0, 0), (3, 5, 0, 0), (4, 6, 0, 0)];
        // id = socket 1 | die 1 | complex 0 | core 5 | thread 1.
        let id = 1 << 6 | 1 << 5 | 5 << 1 | 1;
        let cpu = FakeCpu::new(0x10, 0x8000_0026).with(0x8000_0026, &extended(id, &levels));
        let own = cpu.detect();
        assert_eq!(own.source, Source::Leaf80000026);
        assert_eq!(own.problem, None);
        assert_eq!(own.apic_id, id);
        assert_eq!(
            own.layout.to_string(),
            "thread bits 1, core bits 3, module bits 1, die bits 1, package from bit 6"
        );
        assert_eq!(
            own.place().to_string(),
            "package 1, die 1, module 0, core 5, thread 1"
        );
        assert_eq!(own.layout.threads_per_core(), 2);
        // A non-hybrid leaf reports the core type and no efficiency ranking.
        assert_eq!(
            own.core,
            Some(CoreIdentity {
                core_type: 0,
                native_model: 0,
                efficiency_rank: None,
            })
        );
        assert!(!own.hybrid);
    }

    #[test]
    fn leaf_80000026_reports_core_types_and_efficiency() {
        // HeterogeneousCores at the core level (EAX[30]), EfficiencyRankingAvailable (EAX[29]),
        // core type 3, native model 5, ranking 0x42 (EBX[31:16] = 0x3542).
        let flags = 1 << 30 | 1 << 29;
        let levels = [
            (1, 0, flags, 0x3542),
            (2, 3, 1 << 31, 0),
            (3, 3, 0, 0),
            (4, 5, 0, 0),
        ];
        let cpu = FakeCpu::new(0x10, 0x8000_0026).with(0x8000_0026, &extended(0, &levels));
        let own = cpu.detect();
        assert!(own.hybrid);
        assert_eq!(
            own.core,
            Some(CoreIdentity {
                core_type: 3,
                native_model: 5,
                efficiency_rank: Some(0x42),
            })
        );
        // A complex is the module domain, and a level that is not listed has an empty field.
        assert_eq!(own.layout.width(Domain::Core), 3);
        assert_eq!(own.layout.width(Domain::Module), 0);
        assert_eq!(own.layout.width(Domain::Die), 2);
    }

    #[test]
    fn leaf_80000026_without_a_socket_falls_back() {
        let levels = [(1, 1, 0, 0), (2, 4, 0, 0)];
        let cpu = FakeCpu::new(0x0B, 0x8000_0026)
            .with(0x8000_0026, &extended(3, &levels))
            .with(0x0B, &qemu_0b(3));
        let own = cpu.detect();
        assert_eq!(own.problem, Some(LayoutError::MissingLevel));
        assert_eq!(own.source, Source::Leaf0B);
        // And without a core level, the same.
        let levels = [(2, 4, 0, 0), (4, 6, 0, 0)];
        let cpu = FakeCpu::new(0x0B, 0x8000_0026)
            .with(0x8000_0026, &extended(3, &levels))
            .with(0x0B, &qemu_0b(3));
        assert_eq!(cpu.detect().problem, Some(LayoutError::MissingLevel));
    }

    #[test]
    fn leaf_80000026_zero_level_type_means_it_is_not_implemented() {
        // QEMU 11.1's reply for -cpu max,xlevel=0x80000026: it has no such leaf, and a read
        // inside the range is all zeros. Not a malformed list: the next leaf is taken.
        let cpu = FakeCpu::new(0x0B, 0x8000_0026)
            .with(0x8000_0026, &[[0; 4]])
            .with(0x0B, &qemu_0b(1));
        let own = cpu.detect();
        assert_eq!(own.source, Source::Leaf0B);
        assert_eq!(own.problem, None);
    }

    #[test]
    fn leaf_1a_identifies_a_core_only_when_valid() {
        let mut cpu = FakeCpu::new(0x1F, 0).with(0x1F, &qemu_1f(0));
        // CPUID.07H:EDX[15] set, and leaf 1Ah: core type 0x40, native model 0x123456.
        cpu = cpu.with(7, &[[0, 0, 0, 1 << 15]]);
        cpu = cpu.with(0x1A, &[[0x4012_3456, 0, 0, 0]]);
        let own = cpu.detect();
        assert!(own.hybrid);
        assert_eq!(
            own.core,
            Some(CoreIdentity {
                core_type: 0x40,
                native_model: 0x12_3456,
                efficiency_rank: None,
            })
        );
        // Leaf 1Ah present but zero: invalid (Vol. 1: valid only if EAX is non-zero).
        let plain = FakeCpu::new(0x1F, 0)
            .with(0x1F, &qemu_1f(0))
            .with(0x1A, &[[0, 0, 0, 0]])
            .detect();
        assert_eq!(plain.core, None);
        assert!(!plain.hybrid);
        // Hybrid set, leaf 1Ah absent: reported, for `agree` to catch.
        let unnamed = FakeCpu::new(0x10, 0)
            .with(0x0B, &qemu_0b(0))
            .with(7, &[[0, 0, 0, 1 << 15]])
            .detect();
        assert!(unnamed.hybrid);
        assert_eq!(unnamed.core, None);
    }

    fn own(apic_id: u32, layout_shift: u32) -> CpuTopology {
        let levels = [(1, layout_shift), (2, 4)];
        let cpu = FakeCpu::new(0x1F, 0).with(0x1F, &shifted(apic_id, &levels));
        cpu.detect()
    }

    #[test]
    fn processors_agree_when_each_matches_the_madt() {
        let cpus = [
            (0, Some(own(0, 1))),
            (1, Some(own(1, 1))),
            (4, Some(own(4, 1))),
        ];
        let a = agree(&cpus);
        assert!(a.holds(), "{a:?}");
        assert_eq!(a.checked, 3);
    }

    #[test]
    fn disagreements_are_counted() {
        let bad = |a: Agreement| {
            assert!(!a.holds());
            a
        };
        // A processor that never recorded itself.
        assert_eq!(bad(agree(&[(0, Some(own(0, 1))), (1, None)])).missing, 1);
        // An ID that is not the MADT's.
        assert_eq!(
            bad(agree(&[(0, Some(own(0, 1))), (2, Some(own(1, 1)))])).wrong_apic_id,
            1
        );
        // A different layout from the core domain up.
        let other = {
            let cpu = FakeCpu::new(0x1F, 0).with(0x1F, &shifted(1, &[(1, 1), (2, 5)]));
            cpu.detect()
        };
        assert_eq!(
            bad(agree(&[(0, Some(own(0, 1))), (1, Some(other))])).layout_differs,
            1
        );
        // A different source: 0Bh instead of 1Fh, even with the same boundaries.
        let leaf0b = FakeCpu::new(0x0D, 0)
            .with(0x0B, &shifted(1, &[(1, 1), (2, 4)]))
            .detect();
        assert_eq!(
            bad(agree(&[(0, Some(own(0, 1))), (1, Some(leaf0b))])).layout_differs,
            1
        );
        // A thread field that differs on a non-hybrid machine.
        assert_eq!(
            bad(agree(&[(0, Some(own(0, 1))), (2, Some(own(2, 0)))])).layout_differs,
            1
        );
        // A hybrid processor naming no core type.
        let hybrid_unnamed = FakeCpu::new(0x10, 0)
            .with(0x1F, &shifted(0, &[(1, 1), (2, 4)]))
            .with(7, &[[0, 0, 0, 1 << 15]])
            .detect();
        assert_eq!(bad(agree(&[(0, Some(hybrid_unnamed))])).unidentified, 1);
        // A leaf it claims but cannot be decoded.
        let broken = fallback_after(&[[1, 2, 0x200, 9], [0; 4]]);
        assert_eq!(bad(agree(&[(9, Some(broken))])).undecodable, 1);
        // The same processor twice.
        assert_eq!(
            bad(agree(&[(1, Some(own(1, 1))), (1, Some(own(1, 1)))])).duplicates,
            1
        );
    }

    #[test]
    fn a_hybrid_may_mix_thread_widths_but_nothing_else() {
        // A core with SMT (1 thread bit) and one without (0), the rest identical: the SDM
        // allows either width on a core without SMT.
        let with_type = |id: u32, smt: u32, kind: u32| {
            FakeCpu::new(0x1F, 0)
                .with(0x1F, &shifted(id, &[(1, smt), (2, 4)]))
                .with(7, &[[0, 0, 0, 1 << 15]])
                .with(0x1A, &[[kind << 24 | 1, 0, 0, 0]])
                .detect()
        };
        let with_smt = with_type(0, 1, 0x40);
        let without_smt = with_type(4, 0, 0x20);
        let a = agree(&[(0, Some(with_smt)), (4, Some(without_smt))]);
        assert!(a.holds(), "{a:?}");
        // A core-domain difference is still refused.
        let wide = FakeCpu::new(0x1F, 0)
            .with(0x1F, &shifted(4, &[(1, 1), (2, 5)]))
            .with(7, &[[0, 0, 0, 1 << 15]])
            .with(0x1A, &[[0x2000_0001, 0, 0, 0]])
            .detect();
        assert_eq!(
            agree(&[(0, Some(with_smt)), (4, Some(wide))]).layout_differs,
            1
        );
    }

    #[test]
    fn a_census_counts_each_id_once() {
        let layout = FakeCpu::new(0x1F, 0)
            .with(0x1F, &shifted(0, &[(1, 1), (2, 4)]))
            .detect()
            .layout;
        let mut ids = [17, 16, 1, 0, 0, 16];
        let census = layout.census(&mut ids);
        assert_eq!(census.threads, 4, "the duplicates count once");
        assert_eq!(census.cores, 2);
        assert_eq!(census.packages, 2);
        let mut none = [];
        assert_eq!(layout.census(&mut none), Census::default());
    }

    /// Random layouts through every encoding: 1Fh (with the domains that are present), the
    /// equivalent 80000026h, and 0Bh (which can only see thread and core). The domain IDs a
    /// processor is built from come back, and the census matches a brute-force count.
    #[test]
    fn random_layouts_split_ids_back_into_their_fields() {
        let mut rng = Rng::new(0x70F0_1090);
        for case in 0..cases(3000) {
            // Fields of 0 to 3 bits; a domain with zero bits may or may not be enumerated.
            let widths: [u32; 6] = core::array::from_fn(|_| rng.below(4) as u32);
            let codes = [1u32, 2, 3, 4, 5, 6];
            let mut bound = 0;
            let mut levels = Vec::new();
            let mut bounds = [0u32; 7];
            for (i, (&width, &code)) in widths.iter().zip(&codes).enumerate() {
                bound += width;
                bounds[i + 1] = bound;
                // Thread and core are always listed.
                if i < 2 || width != 0 || rng.below(2) == 0 {
                    levels.push((code, bound));
                }
            }
            // A random ID within the package field too.
            let id = rng.next() as u32 & ((1 << (bound + 2)) - 1);
            let cpu = FakeCpu::new(0x1F, 0).with(0x1F, &shifted(id, &levels));
            let own = cpu.detect();
            assert_eq!(own.source, Source::Leaf1F, "case {case}");
            assert_eq!(own.problem, None, "case {case}");
            for (i, domain) in [
                Domain::Thread,
                Domain::Core,
                Domain::Module,
                Domain::Tile,
                Domain::Die,
                Domain::DieGroup,
            ]
            .into_iter()
            .enumerate()
            {
                let want = (id >> bounds[i]) & ((1 << (bounds[i + 1] - bounds[i])) - 1);
                assert_eq!(own.layout.id(domain, id), want, "case {case} {domain:?}");
            }
            assert_eq!(
                own.layout.id(Domain::Package, id),
                id >> bound,
                "case {case}"
            );

            // The same boundaries through 80000026h (core, complex, die, socket).
            let core_at = bounds[1]; // below the core field
            let complex_at = bounds[2];
            let die_at = bounds[4];
            let socket_at = bounds[6];
            let amd = [
                (1, core_at, 0, 0),
                (2, complex_at, 0, 0),
                (3, die_at, 0, 0),
                (4, socket_at, 0, 0),
            ];
            let cpu = FakeCpu::new(0x10, 0x8000_0026).with(0x8000_0026, &extended(id, &amd));
            let own = cpu.detect();
            assert_eq!(own.source, Source::Leaf80000026, "case {case}");
            assert_eq!(own.layout.shift(Domain::Core), core_at, "case {case}");
            assert_eq!(own.layout.shift(Domain::Package), socket_at, "case {case}");
            assert_eq!(
                own.layout.id(Domain::Die, id),
                (id >> die_at) & ((1 << (socket_at - die_at)) - 1),
                "case {case}"
            );

            // Through 0Bh: thread and core only, core up to the package.
            let cpu = FakeCpu::new(0x0D, 0).with(0x0B, &shifted(id, &[(1, bounds[1]), (2, bound)]));
            let own = cpu.detect();
            assert_eq!(own.source, Source::Leaf0B, "case {case}");
            assert_eq!(own.layout.width(Domain::Die), 0, "case {case}");
            assert_eq!(
                own.layout.width(Domain::Core),
                bound - bounds[1],
                "case {case}"
            );

            // A census over a small random set agrees with a brute-force count.
            let layout = FakeCpu::new(0x1F, 0)
                .with(0x1F, &shifted(0, &levels))
                .detect()
                .layout;
            let set: Vec<u32> = (0..rng.below(40))
                .map(|_| rng.next() as u32 & ((1 << (bound + 2)) - 1))
                .collect();
            let mut scratch = set.clone();
            let census = layout.census(&mut scratch);
            let distinct = |shift: u32| {
                let mut seen: Vec<u32> = set.iter().map(|&i| i >> shift).collect();
                seen.sort_unstable();
                seen.dedup();
                seen.len()
            };
            assert_eq!(census.threads, distinct(0), "case {case}");
            assert_eq!(
                census.cores,
                distinct(layout.shift(Domain::Core)),
                "case {case}"
            );
            assert_eq!(
                census.packages,
                distinct(layout.shift(Domain::Package)),
                "case {case}"
            );
        }
    }
}
