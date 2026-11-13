//! Memory the firmware reserves for devices to reach by DMA (CLAUDE.md §32, §58; ADR-0007 I11
//! and A9, and questions 4 and 8): VT-d's reserved memory regions (RMRRs) and AMD-Vi's
//! memory definitions (IVMDs), and what the kernel does with each.
//!
//! References: VT-d Rev 5.20 (D51397-019) §3.15, §3.16 and §8.4; AMD 48882 Rev 3.11
//! §5.2.2.2 (Tables 111–113), MMIO 0020h and 0028h, and Table 3. Research ledger
//! `docs/research/2026-11-12-reserved-memory-identity-maps.md`, whose questions R1–R5 this
//! follows as the owner accepted them on 2026-11-13 (ADR-0007 Amendment 1, decision items 247
//! to 251).
//!
//! A range becomes a [`Reserved`]: either an identity map (device address = physical
//! address) with the permissions the firmware gives, or a range that must never be mapped
//! for the devices it names. [`check`] decides, once at bring-up, whether an identity range
//! can be trusted: it must lie wholly in memory the memory map calls reserved or EFI runtime
//! (VT-d §8.4 asks the firmware for exactly that), outside the interrupt range and (AMD-Vi)
//! the special address range, clear of every platform register range, inside the domain's
//! address width and at most [`MAX_LENGTH`] long, and, if devices may write it, clear of every
//! firmware table the kernel holds as a reference ([`Lending`]). [`merge`] turns one device's
//! ranges into the disjoint spans its domain maps, at most [`MAX_DEVICE_PAGES`] pages in all.
//! Pure, no `unsafe`; host-tested.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use origin_boot_info::{MemoryRegion, RegionKind};

use crate::ivrs::MemoryRange;
use crate::vtd::{Access, PAGE};

/// The longest identity range accepted: no legacy usage the specifications name needs more,
/// and every 4 KiB costs a page-table entry in each named device's domain (ledger §5).
pub const MAX_LENGTH: u64 = 4 << 30;

/// The most pages one device's identity spans may hold together: as many as one accepted range
/// may, so ranges that overlap or sit side by side cannot add up past the bound one range is
/// refused for (TASKS item 40's review, L4).
pub const MAX_DEVICE_PAGES: u64 = MAX_LENGTH / PAGE;

/// The interrupt address range `FEEx_xxxxh`: nothing may be mapped there (VT-d §3.15; under
/// AMD-Vi the MSI range, §2.2.5).
pub const INTERRUPT_RANGE: Span = Span {
    base: 0xFEE0_0000,
    end: 0xFEF0_0000,
};

/// AMD-Vi's special address range, `FD_0000_0000h`–`FF_FFFF_FFFFh` (48882 Table 3): DTE
/// controls and interrupt tables decide what a device's access there does, not page tables
/// alone.
pub const AMD_SPECIAL_RANGE: Span = Span {
    base: 0xFD_0000_0000,
    end: 0x100_0000_0000,
};

/// A physical (and, identity-mapped, device) address range `[base, end)`, both 4 KiB
/// aligned, `base < end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    pub base: u64,
    pub end: u64,
}

impl Span {
    /// Whether the two ranges share a byte.
    pub fn overlaps(&self, other: &Span) -> bool {
        self.base < other.end && other.base < self.end
    }

    /// Whether `addr` lies inside.
    pub fn contains(&self, addr: u64) -> bool {
        (self.base..self.end).contains(&addr)
    }

    /// Its length in bytes.
    pub fn len(&self) -> u64 {
        self.end - self.base
    }

    /// Whether it is empty (never, for a span this module made).
    pub fn is_empty(&self) -> bool {
        self.end <= self.base
    }
}

/// What a range asks of the domains of the devices it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    /// Map it at device address = physical address, with this access.
    Identity(Access),
    /// Never map it for these devices (an IVMD with IR = IW = 0 and no exclusion).
    NeverMap,
}

/// One firmware-reserved range, as the kernel will use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reserved {
    pub span: Span,
    pub use_: Use,
}

/// Why an identity range is not mapped (and its devices stay refused at attach).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It is longer than [`MAX_LENGTH`].
    TooLong,
    /// It ends beyond what the domain's tables or the platform's addresses reach (bits).
    Width(u8),
    /// It overlaps the interrupt range.
    InterruptRange,
    /// It overlaps AMD-Vi's special address range.
    SpecialRange,
    /// It overlaps the platform register range that starts at this address.
    Registers(u64),
    /// The page at this address is not reserved or EFI runtime memory in the memory map.
    NotReserved(u64),
    /// It is writable and overlaps firmware memory the kernel holds as a reference (an ACPI
    /// table `vm::firmware_bytes` lent), which starts at this address (threat T26).
    Lent(u64),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::TooLong => write!(f, "it is longer than {} GiB", MAX_LENGTH >> 30),
            Self::Width(bits) => write!(f, "it ends beyond {bits}-bit addresses"),
            Self::InterruptRange => f.write_str("it overlaps the interrupt range"),
            Self::SpecialRange => f.write_str("it overlaps AMD-Vi's special address range"),
            Self::Registers(base) => {
                write!(f, "it overlaps the platform registers at {base:#x}")
            }
            Self::NotReserved(at) => write!(
                f,
                "the memory map does not call {at:#x} reserved or EFI runtime memory"
            ),
            Self::Lent(at) => write!(
                f,
                "it is writable and overlaps the firmware table the kernel holds at {at:#x}"
            ),
        }
    }
}

/// An RMRR's range `[base, limit]` (limit inclusive; `dmar::parse` checked both ends are on
/// 4 KiB boundaries and `limit > base`): an identity map, read and write (VT-d §3.16).
pub fn from_rmrr(base: u64, limit: u64) -> Option<Reserved> {
    let end = limit.checked_add(1)?;
    (base.is_multiple_of(PAGE) && end.is_multiple_of(PAGE) && base < end).then_some(Reserved {
        span: Span { base, end },
        use_: Use::Identity(Access::READ_WRITE),
    })
}

/// [`from_rmrr`], with a range it cannot use refused rather than taken for one that maps
/// nothing: its devices then stay refused, as R3 asks of a range beyond the address width
/// (a limit of `u64::MAX` ends past 64-bit addresses; review L1).
pub fn rmrr_range(base: u64, limit: u64) -> Result<Reserved, Refusal> {
    from_rmrr(base, limit).ok_or(Refusal::Width(64))
}

/// An IVMD's range, by its flags (48882 Table 113; ledger R4):
/// - ExclusionRange: an identity map, read and write (the exclusion registers are never
///   used; the device sees the same addresses and access);
/// - IR = IW = 0: never mapped, whether Unity is set or not (Table 113 asks for Unity clear
///   then; set, it is read as R4 reads "IR and IW both clear", not as a reason to refuse the
///   whole table);
/// - otherwise an identity map with exactly IR and IW, with Unity or without it (any
///   translation may then be used, and identity is the one firmware-driven DMA can know).
///
/// The start is rounded down and the end up to 4 KiB (the specification allows rounding the
/// length up). `Ok(None)`: the range is empty. `Err`: it wraps once rounded.
pub fn from_ivmd(m: &MemoryRange) -> Result<Option<Reserved>, Refusal> {
    if m.length == 0 {
        return Ok(None);
    }
    let base = m.start & !(PAGE - 1);
    let end = m
        .start
        .checked_add(m.length)
        .and_then(|e| e.checked_add(PAGE - 1))
        .map(|e| e & !(PAGE - 1))
        .ok_or(Refusal::Width(64))?;
    let use_ = if m.exclusion() {
        Use::Identity(Access::READ_WRITE)
    } else if !m.read() && !m.write() {
        Use::NeverMap
    } else {
        Use::Identity(Access {
            read: m.read(),
            write: m.write(),
        })
    };
    Ok(Some(Reserved {
        span: Span { base, end },
        use_,
    }))
}

/// What [`check`] holds a range against.
#[derive(Clone, Copy, Debug)]
pub struct Platform<'a> {
    /// The memory map the loader passed.
    pub memory_map: &'a [MemoryRegion],
    /// Every platform register range `(base, length)`: the IOMMUs', the local and I/O
    /// APICs', the HPET's and every ECAM window.
    pub registers: &'a [(u64, u64)],
    /// AMD-Vi: [`AMD_SPECIAL_RANGE`] is refused too.
    pub amd: bool,
    /// Every firmware range `[base, end)` the kernel holds as a reference
    /// ([`Lending::lent`]): no device may be allowed to write any of it.
    pub lent: &'a [(u64, u64)],
}

/// Memory-map kinds an identity range may lie in: VT-d §8.4 requires the firmware to report
/// every RMRR as reserved or EFI runtime memory. Nothing the kernel allocates, holds or
/// reclaims is any of these.
pub fn reserved_kind(kind: u32) -> bool {
    matches!(
        RegionKind::from_raw(kind),
        Some(RegionKind::Reserved | RegionKind::RuntimeCode | RegionKind::RuntimeData)
    )
}

/// The first address of `span` that no region of a [`reserved_kind`] covers, if any.
fn first_unreserved(span: Span, map: &[MemoryRegion]) -> Option<u64> {
    let mut at = span.base;
    while at < span.end {
        let Some(covering) = map.iter().find(|r| {
            let end = r.base.saturating_add(r.pages.saturating_mul(PAGE));
            reserved_kind(r.kind) && r.base <= at && at < end
        }) else {
            return Some(at);
        };
        // Progress: `at` lies inside the region, so its end is above `at`.
        at = covering
            .base
            .saturating_add(covering.pages.saturating_mul(PAGE));
    }
    None
}

/// Whether the kernel trusts `r` enough to map it in a domain whose tables translate `width`
/// address bits (the smaller of the tables' reach and the platform's physical addresses).
/// A never-map range maps nothing and is always accepted.
pub fn check(r: &Reserved, width: u8, p: &Platform<'_>) -> Result<(), Refusal> {
    if r.use_ == Use::NeverMap {
        return Ok(());
    }
    let s = r.span;
    if s.len() > MAX_LENGTH {
        return Err(Refusal::TooLong);
    }
    if width < 64 && s.end > 1u64 << width {
        return Err(Refusal::Width(width));
    }
    if s.overlaps(&INTERRUPT_RANGE) {
        return Err(Refusal::InterruptRange);
    }
    if p.amd && s.overlaps(&AMD_SPECIAL_RANGE) {
        return Err(Refusal::SpecialRange);
    }
    if let Some(&(base, _)) = p.registers.iter().find(|&&(base, len)| {
        len != 0
            && s.overlaps(&Span {
                base,
                end: base.saturating_add(len),
            })
    }) {
        return Err(Refusal::Registers(base));
    }
    if let Use::Identity(Access { write: true, .. }) = r.use_
        && let Some(&(base, _)) = p.lent.iter().find(|&&(b, e)| b < s.end && s.base < e)
    {
        return Err(Refusal::Lent(base));
    }
    match first_unreserved(s, p.memory_map) {
        Some(at) => Err(Refusal::NotReserved(at)),
        None => Ok(()),
    }
}

/// Why one device's ranges cannot all be honoured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// An identity range and a never-map range for the device share this page.
    pub at: u64,
}

/// One device's identity spans: the union of its identity ranges, with the union of their
/// permissions where they overlap, sorted, disjoint, and coalesced where neighbours have the
/// same access. A never-map range that shares a page with an identity range is a [`Conflict`]
/// (the firmware asks for both; the device is then refused, as before).
pub fn merge(ranges: &[Reserved]) -> Result<Vec<(Span, Access)>, Conflict> {
    let identity = |r: &Reserved| match r.use_ {
        Use::Identity(a) => Some((r.span, a)),
        Use::NeverMap => None,
    };
    for never in ranges.iter().filter(|r| r.use_ == Use::NeverMap) {
        if let Some((s, _)) = ranges
            .iter()
            .filter_map(identity)
            .find(|(s, _)| s.overlaps(&never.span))
        {
            return Err(Conflict {
                at: s.base.max(never.span.base),
            });
        }
    }
    let mut bounds: Vec<u64> = ranges
        .iter()
        .filter_map(identity)
        .flat_map(|(s, _)| [s.base, s.end])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut out: Vec<(Span, Access)> = Vec::new();
    for w in bounds.windows(2) {
        let piece = Span {
            base: w[0],
            end: w[1],
        };
        let access = ranges
            .iter()
            .filter_map(identity)
            .filter(|(s, _)| s.overlaps(&piece))
            .fold(None, |acc: Option<Access>, (_, a)| {
                Some(match acc {
                    None => a,
                    Some(b) => Access {
                        read: a.read || b.read,
                        write: a.write || b.write,
                    },
                })
            });
        let Some(access) = access else { continue };
        match out.last_mut() {
            Some((last, a)) if last.end == piece.base && *a == access => last.end = piece.end,
            _ => out.push((piece, access)),
        }
    }
    Ok(out)
}

/// Whether `have` (from [`merge`]) maps every page of `need` (from [`merge`]) with at least
/// the access `need` asks: a domain made for one device that another device shares (an
/// AMD-Vi alias) serves the second only if it already holds all of the second's ranges.
pub fn includes(have: &[(Span, Access)], need: &[(Span, Access)]) -> bool {
    need.iter().all(|&(s, a)| {
        let mut at = s.base;
        while at < s.end {
            let Some(&(t, b)) = have.iter().find(|(t, _)| t.contains(at)) else {
                return false;
            };
            if (a.read && !b.read) || (a.write && !b.write) {
                return false;
            }
            at = t.end;
        }
        true
    })
}

/// The never-map spans among `ranges`.
pub fn never_mapped(ranges: &[Reserved]) -> Vec<Span> {
    ranges
        .iter()
        .filter(|r| r.use_ == Use::NeverMap)
        .map(|r| r.span)
        .collect()
}

/// The accepted identity ranges among `ranges`, as `(base, length)` ranges the platform
/// reserves: the BAR check refuses a device's registers wherever another device's DMA reaches
/// untranslated (TASKS item 40's review, L3).
pub fn identity_ranges<'a>(ranges: impl IntoIterator<Item = &'a Reserved>) -> Vec<(u64, u64)> {
    ranges
        .into_iter()
        .filter(|r| matches!(r.use_, Use::Identity(_)))
        .map(|r| (r.span.base, r.span.len()))
        .collect()
}

/// How many pages `identity` (from [`merge`]) maps: `Ok` if no more than
/// [`MAX_DEVICE_PAGES`], `Err` with the count otherwise (the device is then refused).
pub fn bounded(identity: &[(Span, Access)]) -> Result<u64, u64> {
    let total = identity
        .iter()
        .map(|(s, _)| s.len() / PAGE)
        .fold(0u64, u64::saturating_add);
    if total > MAX_DEVICE_PAGES {
        Err(total)
    } else {
        Ok(total)
    }
}

/// Whether the page at `iova` may be unmapped from a domain whose identity spans are
/// `identity`: never one of them (ledger R2).
pub fn may_unmap(identity: &[(Span, Access)], iova: u64) -> bool {
    !identity.iter().any(|(s, _)| s.contains(iova))
}

/// Whether the page at physical `phys` may be mapped for a device whose never-map spans are
/// `never` (an IVMD with IR = IW = 0, R4).
pub fn may_map(never: &[Span], phys: u64) -> bool {
    !never.iter().any(|s| s.contains(phys))
}

/// The firmware memory the kernel lends as `&'static` references (ACPI tables) and the
/// firmware memory devices may write (identity ranges with write access), kept apart: a
/// reference must never cover memory a device can change under it (threat T26). `vm` keeps
/// one under a lock, so a lend and a grant cannot pass each other.
#[derive(Debug, Default)]
pub struct Lending {
    lent: Vec<(u64, u64)>,
    writable: Vec<(u64, u64)>,
}

/// Whether `[base, end)` overlaps a range of `list`.
fn overlaps_any(list: &[(u64, u64)], base: u64, end: u64) -> bool {
    list.iter().any(|&(b, e)| b < end && base < e)
}

impl Lending {
    pub const fn new() -> Self {
        Self {
            lent: Vec::new(),
            writable: Vec::new(),
        }
    }

    /// Records `[base, end)` as lent, unless a device may write any of it: `false` then, and
    /// nothing is recorded.
    pub fn lend(&mut self, base: u64, end: u64) -> bool {
        if overlaps_any(&self.writable, base, end) {
            return false;
        }
        if !self.lent.iter().any(|&(b, e)| b <= base && end <= e) {
            self.lent.push((base, end));
        }
        true
    }

    /// Records that devices may write `[base, end)`, unless any of it is lent: `false` then,
    /// and nothing is recorded.
    pub fn allow_writes(&mut self, base: u64, end: u64) -> bool {
        if overlaps_any(&self.lent, base, end) {
            return false;
        }
        self.writable.push((base, end));
        true
    }

    /// Whether a device may write any of `[base, end)`.
    pub fn device_may_write(&self, base: u64, end: u64) -> bool {
        overlaps_any(&self.writable, base, end)
    }

    /// Every range lent so far.
    pub fn lent(&self) -> &[(u64, u64)] {
        &self.lent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ivrs::kind;

    fn region(base: u64, pages: u64, kind: RegionKind) -> MemoryRegion {
        MemoryRegion {
            base,
            pages,
            kind: kind as u32,
            reserved: 0,
        }
    }

    /// A memory map with RAM, reserved and runtime memory side by side.
    fn map() -> Vec<MemoryRegion> {
        alloc::vec![
            region(0x0, 0x9F, RegionKind::Usable),
            region(0x10_0000, 0x100, RegionKind::Usable),
            region(0x20_0000, 0x10, RegionKind::Reserved),
            region(0x21_0000, 0x10, RegionKind::RuntimeData),
            region(0x22_0000, 0x10, RegionKind::RuntimeCode),
            region(0x23_0000, 0x10, RegionKind::LoaderReclaimable),
            region(0x24_0000, 0x10, RegionKind::AcpiNvs),
            region(0x25_0000, 0x10, RegionKind::Mmio),
            region(0x7A00_0000, 0x100, RegionKind::Reserved),
            region(0xFEC0_0000, 0x1, RegionKind::Mmio),
        ]
    }

    fn platform<'a>(map: &'a [MemoryRegion], registers: &'a [(u64, u64)]) -> Platform<'a> {
        Platform {
            memory_map: map,
            registers,
            amd: false,
            lent: &[],
        }
    }

    fn rw(base: u64, end: u64) -> Reserved {
        Reserved {
            span: Span { base, end },
            use_: Use::Identity(Access::READ_WRITE),
        }
    }

    fn ivmd(kind: u8, flags: u8, start: u64, length: u64) -> MemoryRange {
        MemoryRange {
            offset: 48,
            kind,
            flags,
            device_id: 0x18,
            last_device_id: 0,
            segment: 0,
            start,
            length,
            unit_base: 0xFED8_0000,
            unit_segment: 0,
        }
    }

    #[test]
    fn an_rmrr_is_read_and_write_identity_over_its_inclusive_limit() {
        let r = from_rmrr(0x7A00_0000, 0x7A0F_FFFF).unwrap();
        assert_eq!(
            r.span,
            Span {
                base: 0x7A00_0000,
                end: 0x7A10_0000
            }
        );
        assert_eq!(r.use_, Use::Identity(Access::READ_WRITE));
        // `dmar::parse` refuses these; this module does not trust that it ran.
        assert_eq!(from_rmrr(0x1000, 0x1000), None);
        assert_eq!(from_rmrr(0x1001, 0x1FFF), None);
        assert_eq!(from_rmrr(0x2000, 0x0FFF), None);
        assert_eq!(from_rmrr(0, u64::MAX), None);
    }

    #[test]
    fn an_rmrr_the_kernel_cannot_use_is_refused_not_taken_as_empty() {
        // `dmar::parse` accepts this one (aligned, limit above base); its end does not fit.
        assert_eq!(
            rmrr_range(0xFFFF_FFFF_FFFF_F000, u64::MAX),
            Err(Refusal::Width(64))
        );
        assert_eq!(rmrr_range(0x2000, 0x0FFF), Err(Refusal::Width(64)));
        assert_eq!(
            rmrr_range(0x7A00_0000, 0x7A00_0FFF).map(|r| r.span),
            Ok(Span {
                base: 0x7A00_0000,
                end: 0x7A00_1000
            })
        );
    }

    #[test]
    fn ivmd_flags_choose_the_use() {
        let use_of = |flags| from_ivmd(&ivmd(kind::IVMD_DEVICE, flags, 0x1000, 0x1000)).unwrap();
        let id = |read, write| Some(Use::Identity(Access { read, write }));
        // Unity with each permission, and the permissions without Unity.
        assert_eq!(use_of(0b0111).map(|r| r.use_), id(true, true));
        assert_eq!(use_of(0b0011).map(|r| r.use_), id(true, false));
        assert_eq!(use_of(0b0101).map(|r| r.use_), id(false, true));
        assert_eq!(use_of(0b0110).map(|r| r.use_), id(true, true));
        assert_eq!(use_of(0b0010).map(|r| r.use_), id(true, false));
        // IR = IW = 0 and no exclusion: never mapped, with Unity set as well (review I2).
        assert_eq!(use_of(0b0000).map(|r| r.use_), Some(Use::NeverMap));
        assert_eq!(use_of(0b0001).map(|r| r.use_), Some(Use::NeverMap));
        // ExclusionRange: read and write, whatever IR, IW and Unity say.
        for flags in [0b1000, 0b1001, 0b1010, 0b1111] {
            assert_eq!(use_of(flags).map(|r| r.use_), id(true, true), "{flags:#b}");
        }
    }

    #[test]
    fn ivmd_ranges_are_rounded_out_to_whole_pages() {
        let r = from_ivmd(&ivmd(kind::IVMD_ALL, 0b0111, 0x1234, 0x10))
            .unwrap()
            .unwrap();
        assert_eq!(
            r.span,
            Span {
                base: 0x1000,
                end: 0x2000
            }
        );
        let r = from_ivmd(&ivmd(kind::IVMD_ALL, 0b0111, 0x1FFF, 2))
            .unwrap()
            .unwrap();
        assert_eq!(
            r.span,
            Span {
                base: 0x1000,
                end: 0x3000
            }
        );
        let r = from_ivmd(&ivmd(kind::IVMD_ALL, 0b0111, 0x4000, 0x1000))
            .unwrap()
            .unwrap();
        assert_eq!(
            r.span,
            Span {
                base: 0x4000,
                end: 0x5000
            }
        );
        assert_eq!(
            from_ivmd(&ivmd(kind::IVMD_ALL, 0b0111, 0x4000, 0)),
            Ok(None)
        );
        // Ends in the last page of the address space: rounding up wraps.
        assert_eq!(
            from_ivmd(&ivmd(kind::IVMD_ALL, 0b0111, u64::MAX - 0x1FFF, 0x1001)),
            Err(Refusal::Width(64))
        );
    }

    #[test]
    fn a_range_in_reserved_or_runtime_memory_is_accepted() {
        let m = map();
        let p = platform(&m, &[]);
        assert_eq!(check(&rw(0x7A00_0000, 0x7A10_0000), 48, &p), Ok(()));
        // Across three adjacent regions of the three kinds.
        assert_eq!(check(&rw(0x20_0000, 0x23_0000), 48, &p), Ok(()));
        assert_eq!(check(&rw(0x21_F000, 0x22_1000), 48, &p), Ok(()));
    }

    #[test]
    fn a_range_outside_reserved_memory_is_refused_at_its_first_such_page() {
        let m = map();
        let p = platform(&m, &[]);
        // RAM the kernel allocates, reclaims, or reads, and device memory.
        assert_eq!(
            check(&rw(0x10_0000, 0x10_1000), 48, &p),
            Err(Refusal::NotReserved(0x10_0000))
        );
        assert_eq!(
            check(&rw(0x22_F000, 0x23_1000), 48, &p),
            Err(Refusal::NotReserved(0x23_0000))
        );
        assert_eq!(
            check(&rw(0x24_0000, 0x24_1000), 48, &p),
            Err(Refusal::NotReserved(0x24_0000))
        );
        assert_eq!(
            check(&rw(0x25_0000, 0x25_1000), 48, &p),
            Err(Refusal::NotReserved(0x25_0000))
        );
        // Memory the map does not describe at all: a gap after the last reserved page.
        assert_eq!(
            check(&rw(0x7A0F_F000, 0x7A10_1000), 48, &p),
            Err(Refusal::NotReserved(0x7A10_0000))
        );
        assert_eq!(
            check(&rw(0x9000_0000, 0x9000_1000), 48, &p),
            Err(Refusal::NotReserved(0x9000_0000))
        );
        // An empty memory map trusts nothing.
        assert_eq!(
            check(&rw(0x7A00_0000, 0x7A00_1000), 48, &platform(&[], &[])),
            Err(Refusal::NotReserved(0x7A00_0000))
        );
    }

    #[test]
    fn the_interrupt_range_and_amds_special_range_are_refused() {
        let mut m = map();
        m.push(region(0xFEE0_0000, 0x100, RegionKind::Reserved));
        m.push(region(0xFD_0000_0000, 0x10, RegionKind::Reserved));
        let p = platform(&m, &[]);
        assert_eq!(
            check(&rw(0xFEEF_F000, 0xFEF0_0000), 48, &p),
            Err(Refusal::InterruptRange)
        );
        assert_eq!(
            check(&rw(0xFEE0_0000, 0xFEE0_1000), 48, &p),
            Err(Refusal::InterruptRange)
        );
        // Not refused as a special range under VT-d, but by the map (it is reserved here).
        assert_eq!(check(&rw(0xFD_0000_0000, 0xFD_0000_1000), 48, &p), Ok(()));
        let amd = Platform { amd: true, ..p };
        assert_eq!(
            check(&rw(0xFD_0000_0000, 0xFD_0000_1000), 48, &amd),
            Err(Refusal::SpecialRange)
        );
    }

    #[test]
    fn platform_registers_are_refused() {
        let mut m = map();
        m.push(region(0xFED9_0000, 0x10, RegionKind::Reserved));
        let registers = [(0xFEC0_0000, 0x1000), (0xFED9_0000, 0x1000)];
        let p = platform(&m, &registers);
        assert_eq!(
            check(&rw(0xFED8_F000, 0xFED9_1000), 48, &p),
            Err(Refusal::Registers(0xFED9_0000))
        );
        assert_eq!(check(&rw(0xFED9_1000, 0xFED9_2000), 48, &p), Ok(()));
        // A zero-length register range names nothing.
        let empty = [(0xFED9_1000, 0)];
        assert_eq!(
            check(&rw(0xFED9_1000, 0xFED9_2000), 48, &platform(&m, &empty)),
            Ok(())
        );
    }

    #[test]
    fn width_and_length_bound_a_range() {
        let mut m = map();
        m.push(region(1 << 39, 0x10, RegionKind::Reserved));
        m.push(region(
            0x1_0000_0000,
            (MAX_LENGTH / PAGE) + 1,
            RegionKind::Reserved,
        ));
        let p = platform(&m, &[]);
        let high = rw(1 << 39, (1 << 39) + PAGE);
        assert_eq!(check(&high, 39, &p), Err(Refusal::Width(39)));
        assert_eq!(check(&high, 48, &p), Ok(()));
        let last = rw((1 << 39) - PAGE, 1 << 39);
        assert_eq!(
            check(&last, 39, &p),
            Err(Refusal::NotReserved((1 << 39) - PAGE))
        );
        let longest = rw(0x1_0000_0000, 0x1_0000_0000 + MAX_LENGTH);
        assert_eq!(check(&longest, 48, &p), Ok(()));
        let longer = rw(0x1_0000_0000, 0x1_0000_0000 + MAX_LENGTH + PAGE);
        assert_eq!(check(&longer, 48, &p), Err(Refusal::TooLong));
    }

    #[test]
    fn a_never_map_range_is_always_accepted() {
        let never = Reserved {
            span: INTERRUPT_RANGE,
            use_: Use::NeverMap,
        };
        assert_eq!(check(&never, 39, &platform(&[], &[])), Ok(()));
    }

    #[test]
    fn merge_unions_spans_and_permissions() {
        let r = |base, end, read, write| Reserved {
            span: Span { base, end },
            use_: Use::Identity(Access { read, write }),
        };
        let spans = merge(&[
            r(0x3000, 0x5000, true, false),
            r(0x1000, 0x2000, true, true),
            r(0x4000, 0x6000, false, true),
            r(0x2000, 0x3000, true, true),
        ])
        .unwrap();
        let s = |base, end| Span { base, end };
        assert_eq!(
            spans,
            alloc::vec![
                (s(0x1000, 0x3000), Access::READ_WRITE),
                (s(0x3000, 0x4000), Access::READ),
                (s(0x4000, 0x5000), Access::READ_WRITE),
                (s(0x5000, 0x6000), Access::WRITE),
            ]
        );
        assert_eq!(merge(&[]).unwrap(), alloc::vec![]);
        // The same range twice is mapped once.
        assert_eq!(
            merge(&[rw(0x1000, 0x2000), rw(0x1000, 0x2000)]).unwrap(),
            alloc::vec![(s(0x1000, 0x2000), Access::READ_WRITE)]
        );
    }

    #[test]
    fn identity_and_never_map_on_one_page_conflict() {
        let never = |base, end| Reserved {
            span: Span { base, end },
            use_: Use::NeverMap,
        };
        assert_eq!(
            merge(&[rw(0x1000, 0x3000), never(0x2000, 0x4000)]),
            Err(Conflict { at: 0x2000 })
        );
        // Side by side is fine, and the never-map range maps nothing.
        let ranges = [rw(0x1000, 0x2000), never(0x2000, 0x4000)];
        assert_eq!(
            merge(&ranges).unwrap(),
            alloc::vec![(
                Span {
                    base: 0x1000,
                    end: 0x2000
                },
                Access::READ_WRITE
            )]
        );
        assert_eq!(
            never_mapped(&ranges),
            alloc::vec![Span {
                base: 0x2000,
                end: 0x4000
            }]
        );
    }

    #[test]
    fn includes_asks_for_every_page_and_its_access() {
        let s = |base, end| Span { base, end };
        let have = [
            (s(0x1000, 0x3000), Access::READ_WRITE),
            (s(0x3000, 0x4000), Access::READ),
        ];
        assert!(includes(&have, &[]));
        assert!(includes(&have, &[(s(0x1000, 0x4000), Access::READ)]));
        assert!(includes(&have, &[(s(0x2000, 0x3000), Access::WRITE)]));
        assert!(!includes(&have, &[(s(0x2000, 0x4000), Access::WRITE)]));
        assert!(!includes(&have, &[(s(0x0, 0x2000), Access::READ)]));
        assert!(!includes(&have, &[(s(0x3000, 0x5000), Access::READ)]));
        assert!(!includes(&[], &[(s(0x1000, 0x2000), Access::READ)]));
    }

    /// Every span `merge` returns is page aligned, disjoint, sorted, never empty, and covers
    /// exactly the pages some identity range covers, with exactly their union of access.
    #[test]
    fn merge_agrees_with_a_page_by_page_model() {
        let mut rng = crate::testing::Rng::new(0x5E5E_4E4E);
        for _ in 0..2_000 {
            let n = 1 + rng.below(5);
            let ranges: Vec<Reserved> = (0..n)
                .map(|_| {
                    let base = rng.below(16) as u64 * PAGE;
                    let end = base + (1 + rng.below(6) as u64) * PAGE;
                    let (read, write) = match rng.below(3) {
                        0 => (true, false),
                        1 => (false, true),
                        _ => (true, true),
                    };
                    Reserved {
                        span: Span { base, end },
                        use_: Use::Identity(Access { read, write }),
                    }
                })
                .collect();
            let spans = merge(&ranges).unwrap();
            for w in spans.windows(2) {
                assert!(w[0].0.end <= w[1].0.base);
                assert!(!(w[0].0.end == w[1].0.base && w[0].1 == w[1].1));
            }
            for page in 0..24u64 {
                let at = page * PAGE;
                let want = ranges.iter().filter(|r| r.span.contains(at)).fold(
                    None,
                    |acc: Option<Access>, r| {
                        let Use::Identity(a) = r.use_ else {
                            unreachable!()
                        };
                        Some(acc.map_or(a, |b| Access {
                            read: a.read || b.read,
                            write: a.write || b.write,
                        }))
                    },
                );
                let got = spans.iter().find(|(s, _)| s.contains(at)).map(|(_, a)| *a);
                assert_eq!(got, want, "page {page} of {ranges:?}");
            }
            assert!(spans.iter().all(|(s, _)| {
                !s.is_empty() && s.base.is_multiple_of(PAGE) && s.end.is_multiple_of(PAGE)
            }));
        }
    }

    #[test]
    fn a_writable_range_over_a_lent_table_is_refused() {
        let m = map();
        // A 128-byte DMAR inside the reserved region at 0x7A00_0000, lent as a reference.
        let lent = [(0x7A00_0040, 0x7A00_00C0)];
        let p = Platform {
            lent: &lent,
            ..platform(&m, &[])
        };
        let over = rmrr_range(0x7A00_0000, 0x7A00_0FFF).unwrap();
        assert_eq!(check(&over, 48, &p), Err(Refusal::Lent(0x7A00_0040)));
        // Read only, or never mapped: a device cannot change the table.
        let read_only = Reserved {
            use_: Use::Identity(Access {
                read: true,
                write: false,
            }),
            ..over
        };
        assert_eq!(check(&read_only, 48, &p), Ok(()));
        let never = Reserved {
            use_: Use::NeverMap,
            ..over
        };
        assert_eq!(check(&never, 48, &p), Ok(()));
        // The next page holds no lent byte.
        let next = rmrr_range(0x7A00_1000, 0x7A00_1FFF).unwrap();
        assert_eq!(check(&next, 48, &p), Ok(()));
        // A range that ends where the table starts, or starts where it ends, does not overlap.
        let edge = [(0x7A00_1000, 0x7A00_1080)];
        let p = Platform {
            lent: &edge,
            ..platform(&m, &[])
        };
        assert_eq!(check(&over, 48, &p), Ok(()));
        assert_eq!(
            check(&rmrr_range(0x7A00_1000, 0x7A00_1FFF).unwrap(), 48, &p),
            Err(Refusal::Lent(0x7A00_1000))
        );
    }

    #[test]
    fn lent_memory_and_device_writable_memory_never_overlap() {
        // A table lent first: a grant of writes over any of it is refused.
        let mut l = Lending::new();
        assert!(l.lend(0x7A00_0040, 0x7A00_00C0));
        assert!(!l.allow_writes(0x7A00_0000, 0x7A00_1000));
        assert!(!l.allow_writes(0x7A00_00BF, 0x7A00_00C0));
        assert!(l.allow_writes(0x7A00_00C0, 0x7A00_1000));
        assert!(l.allow_writes(0x7A00_0000, 0x7A00_0040));
        assert!(l.device_may_write(0x7A00_0000, 0x7A00_0001));
        assert!(!l.device_may_write(0x7A00_0040, 0x7A00_00C0));
        // Writes granted first: a lend over any of it is refused, and recorded nowhere.
        assert!(!l.lend(0x7A00_0F00, 0x7A00_1000));
        assert!(!l.lend(0x7A00_0000, 0x7A00_0041));
        assert_eq!(l.lent(), &[(0x7A00_0040, 0x7A00_00C0)]);
        // Lending the same table again records it once.
        assert!(l.lend(0x7A00_0040, 0x7A00_00C0));
        assert!(l.lend(0x7A00_0050, 0x7A00_0060));
        assert_eq!(l.lent(), &[(0x7A00_0040, 0x7A00_00C0)]);
        assert!(l.lend(0x7A00_2000, 0x7A00_2010));
        assert_eq!(l.lent().len(), 2);
    }

    #[test]
    fn a_devices_identity_pages_are_bounded_in_all() {
        let span = |base: u64, pages: u64| Span {
            base,
            end: base + pages * PAGE,
        };
        let at_bound = [
            (span(0, MAX_DEVICE_PAGES / 2), Access::READ_WRITE),
            (span(1 << 40, MAX_DEVICE_PAGES / 2), Access::READ_WRITE),
        ];
        assert_eq!(bounded(&at_bound), Ok(MAX_DEVICE_PAGES));
        let past = [
            (span(0, MAX_DEVICE_PAGES / 2), Access::READ_WRITE),
            (span(1 << 40, MAX_DEVICE_PAGES / 2 + 1), Access::READ_WRITE),
        ];
        assert_eq!(bounded(&past), Err(MAX_DEVICE_PAGES + 1));
        assert_eq!(bounded(&[]), Ok(0));
        // Each range alone within MAX_LENGTH: merged, they still count once per page.
        let ranges = [
            rw(0x2000_0000, 0x2000_0000 + MAX_LENGTH),
            rw(0x2000_0000, 0x2000_1000),
        ];
        assert_eq!(bounded(&merge(&ranges).unwrap()), Ok(MAX_DEVICE_PAGES));
    }

    #[test]
    fn identity_pages_are_never_unmapped_and_never_map_pages_never_mapped() {
        let identity = [(
            Span {
                base: 0x7A00_0000,
                end: 0x7A00_2000,
            },
            Access::READ_WRITE,
        )];
        assert!(may_unmap(&identity, 0x79FF_F000));
        assert!(!may_unmap(&identity, 0x7A00_0000));
        assert!(!may_unmap(&identity, 0x7A00_1000));
        assert!(may_unmap(&identity, 0x7A00_2000));
        assert!(may_unmap(&[], 0x7A00_0000));
        let never = [Span {
            base: 0x20_0000,
            end: 0x20_3000,
        }];
        assert!(may_map(&never, 0x1F_F000));
        assert!(!may_map(&never, 0x20_0000));
        assert!(!may_map(&never, 0x20_2000));
        assert!(may_map(&never, 0x20_3000));
        assert!(may_map(&[], 0x20_0000));
    }
}
