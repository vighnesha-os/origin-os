//! Capacity: how much work each processor does per unit of time, relative to the most capable
//! processor of the machine, from the hardware's own reports only (ADR-0020 S9; CLAUDE.md §27,
//! matrix row S27-10; TASKS item 97).
//!
//! The sources, in the order S9 takes them, the first that gives a value for **every** online
//! processor being used for all of them, so that every capacity is on one scale (the coordinating
//! session's ruling Q10 of 2026-11-21):
//!
//! 1. **Intel's Hardware Feedback Interface:** class 0's performance capability in the processor's
//!    row of its package's table (Intel SDM Vol. 3, §17.6.1 and §17.6.2, Tables 17-4 to 17-9).
//! 2. **ACPI `_CPC`'s Highest Performance**, an Integer or a register (ACPI 6.6 §8.4.6.1,
//!    Table 8.23).
//! 3. **The processor's own register:** Intel's IA32_HWP_CAPABILITIES Highest (bits 7:0, SDM Vol.
//!    3, §17.4.3), or AMD's CPPC_CAPABILITY_1 HighestPerf (bits 31:24, APM Vol. 2, §17.6.4.1).
//! 4. **AMD's efficiency ranking** (Fn8000_0026 EBX\[23:16\], APM Vol. 3, §E.4.24) is an order
//!    among cores of one core type, not a ratio: it breaks ties ([`Ranking`]) and never gives a
//!    capacity.
//! 5. **Otherwise every processor is [`FULL`].**
//!
//! **No capacity is ever taken from a core-type code or a model name** (S27-10): Intel says a core
//! type's value implies no attribute, and AMD's native model ID does not even tell Zen 5 from Zen
//! 5c. A core type is used here only for equality, to say which processors a ranking compares.
//!
//! Everything here is a pure function over values given to it. Reading the HFI table, evaluating
//! `_CPC`, reading the registers and taking the thermal interrupt are TASKS item 106's; until it
//! lands, the kernel gives every processor [`FULL`] (source 5), which is all QEMU can show.
#![forbid(unsafe_code)]

use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::topology::CoreIdentity;

/// The most capable processor's capacity.
pub const FULL: u16 = 1024;

/// Which source the capacities came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The Hardware Feedback Interface (1).
    Hfi,
    /// ACPI `_CPC`'s Highest Performance (2).
    Cpc,
    /// IA32_HWP_CAPABILITIES or CPPC_CAPABILITY_1 (3).
    Register,
    /// None: every processor is [`FULL`] (5).
    Equal,
}

/// Every processor [`FULL`]: source 5.
pub fn equal(processors: usize) -> Vec<u16> {
    vec![FULL; processors]
}

/// Scales `values` so the largest is [`FULL`]. A value of 0 stays 0; any other is at least 1.
fn scale(values: &[u32]) -> Option<Vec<u16>> {
    let top = u64::from(*values.iter().max()?);
    if top == 0 {
        return None;
    }
    Some(
        values
            .iter()
            .map(|&v| {
                let scaled = u64::from(v) * u64::from(FULL) / top;
                match (v, scaled) {
                    (0, _) => 0,
                    (_, 0) => 1,
                    (_, s) => s as u16,
                }
            })
            .collect(),
    )
}

/// Every processor's value, if the source gives one for each of the `n` processors.
fn whole(n: usize, values: Option<&[Option<u32>]>) -> Option<Vec<u32>> {
    let values = values?;
    if values.len() != n || n == 0 {
        return None;
    }
    values.iter().copied().collect()
}

/// What each source gives each processor, in CPU-index order; a source a processor gives
/// nothing for is `None` there, and a source that does not exist is `None` as a whole.
#[derive(Clone, Copy, Debug, Default)]
pub struct Reports<'a> {
    /// Class 0's performance capability in each processor's HFI row: 0 means "avoid".
    pub hfi: Option<&'a [Option<u32>]>,
    /// Each processor's `_CPC` Highest Performance.
    pub cpc: Option<&'a [Option<u32>]>,
    /// Each processor's register Highest.
    pub register: Option<&'a [Option<u32>]>,
}

/// The capacities of `n` processors: the first source that gives every one a value, scaled so
/// the largest is [`FULL`].
///
/// - **HFI:** a 0 is kept as capacity 0 ("avoid": no movable thread is placed there). A table the
///   processor has not written yet is all 0 (the operating system zeroes it before enabling it):
///   the caller gives `None` for every processor of a package whose table has not yet been
///   written (its status bit says so, SDM Vol. 3 §17.6.5), so one package's unwritten table never
///   reads as "avoid" (the review's R97-14; TASKS item 106 keeps that contract). Every value 0
///   across the machine is no source either.
/// - **`_CPC` and the register:** a Highest of 0 is no value; that processor has none, and the
///   source is passed over.
pub fn choose(n: usize, reports: &Reports<'_>) -> (Source, Vec<u16>) {
    if let Some(values) = whole(n, reports.hfi).and_then(|v| scale(&v)) {
        return (Source::Hfi, values);
    }
    let positive = |v: Vec<u32>| v.iter().all(|&x| x > 0).then_some(v);
    if let Some(values) = whole(n, reports.cpc)
        .and_then(positive)
        .and_then(|v| scale(&v))
    {
        return (Source::Cpc, values);
    }
    if let Some(values) = whole(n, reports.register)
        .and_then(positive)
        .and_then(|v| scale(&v))
    {
        return (Source::Register, values);
    }
    (Source::Equal, equal(n))
}

/// The shape of a package's HFI or Thread Director table, from CPUID.06H (SDM Vol. 1, Table 21-16;
/// Vol. 3, §17.6.2): how many capabilities each class has, and how many classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HfiShape {
    /// The capabilities enumerated in EDX\[7:0\], as a mask: bit 0 performance, bit 1 efficiency.
    capabilities: u8,
    /// Classes: ECX\[15:8\] with Thread Director, one without.
    classes: u8,
}

/// CPUID.06H:EAX\[19\], the Hardware Feedback Interface.
const HFI: u32 = 1 << 19;
/// CPUID.06H:EAX\[23\], Thread Director.
const THREAD_DIRECTOR: u32 = 1 << 23;
/// A table's capability for performance (EDX bit 0) and for efficiency (bit 1).
const PERFORMANCE: u8 = 1;
const EFFICIENCY: u8 = 2;

impl HfiShape {
    /// The shape CPUID.06H's `eax`, `ecx` and `edx` describe, if the processor has an HFI that
    /// reports performance.
    pub const fn from_cpuid(eax: u32, ecx: u32, edx: u32) -> Option<HfiShape> {
        let capabilities = (edx & 0xFF) as u8;
        if eax & HFI == 0 || capabilities & PERFORMANCE == 0 {
            return None;
        }
        let classes = if eax & THREAD_DIRECTOR != 0 {
            ((ecx >> 8) & 0xFF) as u8
        } else {
            1
        };
        Some(HfiShape {
            capabilities,
            classes: if classes == 0 { 1 } else { classes },
        })
    }

    /// Bytes of one class's capabilities.
    const fn per_class(&self) -> usize {
        self.capabilities.count_ones() as usize
    }

    /// Bytes of the change indications or of one row: every class's capabilities, rounded up to
    /// 8.
    const fn stride(&self) -> usize {
        (self.per_class() * self.classes as usize).div_ceil(8) * 8
    }

    /// Where capability `mask` sits within a class: after the enumerated capabilities below it.
    const fn place(&self, mask: u8) -> Option<usize> {
        if self.capabilities & mask == 0 {
            return None;
        }
        Some((self.capabilities & (mask - 1)).count_ones() as usize)
    }
}

/// The table's global flags for class 0 (Table 17-5, 17-8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HfiFlags {
    /// A performance capability changed.
    pub performance_changed: bool,
    /// Idle is to be forced or injected on one or more processors, by the performance flags.
    pub performance_force_idle: bool,
    /// An efficiency capability changed.
    pub efficiency_changed: bool,
    /// Idle is to be forced or injected, by the efficiency flags.
    pub efficiency_force_idle: bool,
}

/// One processor's row, class 0 (Tables 17-6 and 17-9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HfiRow {
    /// Relative performance, 0 to 255; 0 asks that no thread run there for performance.
    pub performance: u8,
    /// Relative efficiency, where the table reports it; 255 asks that work be consolidated there.
    pub efficiency: Option<u8>,
}

impl HfiRow {
    /// Every class-0 cell is 0: "avoid" the processor altogether (SDM Vol. 3, §17.6.7).
    pub fn avoid(&self) -> bool {
        self.performance == 0 && self.efficiency.unwrap_or(0) == 0
    }

    /// Efficiency 255: a request to consolidate work on the processors that report it (S17).
    pub fn consolidate(&self) -> bool {
        self.efficiency == Some(255)
    }
}

/// The flags at the table's start, if `table` holds its header.
pub fn hfi_flags(table: &[u8], shape: HfiShape) -> Option<HfiFlags> {
    let byte = |mask| -> Option<u8> { table.get(8 + shape.place(mask)?).copied() };
    let performance = table.get(8 + shape.place(PERFORMANCE)?).copied()?;
    let efficiency = byte(EFFICIENCY).unwrap_or(0);
    Some(HfiFlags {
        performance_changed: performance & 1 != 0,
        performance_force_idle: performance & 2 != 0,
        efficiency_changed: efficiency & 1 != 0,
        efficiency_force_idle: efficiency & 2 != 0,
    })
}

/// Row `row`'s class-0 capabilities (CPUID.06H:EDX\[31:16\] on that processor), if `table` holds
/// it: at 8 + (row + 1) × stride (Table 17-7's note 3, which for the HFI's one class and two
/// capabilities is Table 17-4's 16 + 8 × row).
pub fn hfi_row(table: &[u8], shape: HfiShape, row: u16) -> Option<HfiRow> {
    let start = (usize::from(row) + 1)
        .checked_mul(shape.stride())?
        .checked_add(8)?;
    let cell = |mask| -> Option<u8> { table.get(start.checked_add(shape.place(mask)?)?).copied() };
    Some(HfiRow {
        performance: cell(PERFORMANCE)?,
        efficiency: match shape.place(EFFICIENCY) {
            Some(_) => Some(cell(EFFICIENCY)?),
            None => None,
        },
    })
}

/// A `_CPC` package element, as the AML interpreter evaluates it (TASKS item 106 converts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpcElement<'a> {
    /// An Integer.
    Integer(u64),
    /// A Buffer: for a register field, a resource template holding one Register().
    Buffer(&'a [u8]),
    /// A Package, or anything else.
    Other,
}

/// A register a `_CPC` field names: ACPI 6.6's Generic Register Descriptor (§6.4.3.7, Table 6.53).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpcRegister {
    /// The address space (0 memory, 1 I/O, 0x0A PCC, 0x7F functional fixed hardware, …).
    pub space: u8,
    /// The register's width in bits.
    pub bit_width: u8,
    /// Its first bit at the address.
    pub bit_offset: u8,
    /// The access size: 0 undefined, 1 byte to 4 quad word.
    pub access_size: u8,
    /// Its address in that space.
    pub address: u64,
}

/// Why a `_CPC` package gives no Highest Performance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpcError {
    /// Fewer than three elements, or NumEntries is not an Integer equal to the package's length.
    NumEntries,
    /// The revision is not an Integer, or is not one of [`CPC_REVISIONS`].
    Revision,
    /// Highest Performance is neither an Integer nor a register.
    Highest,
    /// The buffer is not one Generic Register Descriptor, or its field does not fit the register:
    /// a width of 0, or an offset and width past 64 bits or past its access size.
    Descriptor,
    /// The NULL register, which marks a field unsupported: Highest is not optional.
    NullRegister,
    /// The register could not be read.
    Unread,
    /// A value wider than the 32 bits Highest may have, or of 0.
    Value,
}

/// The Generic Register Descriptor at the start of a resource-template buffer.
pub fn generic_register(buffer: &[u8]) -> Result<CpcRegister, CpcError> {
    let d = buffer.get(..15).ok_or(CpcError::Descriptor)?;
    if d[0] != 0x82 || u16::from_le_bytes([d[1], d[2]]) != 12 {
        return Err(CpcError::Descriptor);
    }
    let mut address = [0u8; 8];
    address.copy_from_slice(&d[7..15]);
    Ok(CpcRegister {
        space: d[3],
        bit_width: d[4],
        bit_offset: d[5],
        access_size: d[6],
        address: u64::from_le_bytes(address),
    })
}

/// The `_CPC` revisions whose layout is known here. ACPI 6.6 gives revision 4 with 25 entries;
/// for 2 and 3 no document read gives the count (the coordinating session's ruling Q10), so only
/// NumEntries against the package's length, and Highest's place, are checked.
pub const CPC_REVISIONS: [u64; 3] = [2, 3, 4];

/// `_CPC`'s Highest Performance: element 2, an Integer read directly, or a register read through
/// `read`, which gives the whole register, and the field its descriptor names taken from it: the
/// bit width at the bit offset (the review's R97-5). NumEntries (element 0) must equal the
/// package's length, and the revision must be one of [`CPC_REVISIONS`].
pub fn cpc_highest(
    package: &[CpcElement<'_>],
    read: impl Fn(&CpcRegister) -> Option<u64>,
) -> Result<u32, CpcError> {
    match package.first() {
        Some(CpcElement::Integer(n)) if usize::try_from(*n) == Ok(package.len()) && *n >= 3 => {}
        _ => return Err(CpcError::NumEntries),
    }
    match package.get(1) {
        Some(CpcElement::Integer(revision)) if CPC_REVISIONS.contains(revision) => {}
        _ => return Err(CpcError::Revision),
    }
    let value = match package.get(2) {
        Some(CpcElement::Integer(v)) => *v,
        Some(CpcElement::Buffer(b)) => {
            let register = generic_register(b)?;
            if register.space == 0 && register.bit_width == 0 && register.address == 0 {
                return Err(CpcError::NullRegister);
            }
            let (offset, width) = (
                u32::from(register.bit_offset),
                u32::from(register.bit_width),
            );
            let limit = match register.access_size {
                size @ 1..=4 => 8 << (size - 1),
                _ => 64,
            };
            if width == 0 || offset + width > limit {
                return Err(CpcError::Descriptor);
            }
            let field = read(&register).ok_or(CpcError::Unread)? >> offset;
            if width == 64 {
                field
            } else {
                field & ((1u64 << width) - 1)
            }
        }
        _ => return Err(CpcError::Highest),
    };
    match u32::try_from(value) {
        Ok(v) if v > 0 => Ok(v),
        _ => Err(CpcError::Value),
    }
}

/// Whether a Notify 0x85 on the processor makes the operating system read Highest again: only
/// where it is a register, and `_CPC` itself is never evaluated again (ACPI 6.6 §8.4.6.1.1.1).
pub fn cpc_rereads_highest(package: &[CpcElement<'_>]) -> bool {
    matches!(package.get(2), Some(CpcElement::Buffer(_)))
}

/// IA32_HWP_CAPABILITIES' Highest_Performance, bits 7:0.
pub const fn hwp_highest(capabilities: u64) -> u32 {
    (capabilities & 0xFF) as u32
}

/// CPPC_CAPABILITY_1's HighestPerf, bits 31:24.
pub const fn amd_cppc_highest(capability_1: u64) -> u32 {
    ((capability_1 >> 24) & 0xFF) as u32
}

/// AMD's static efficiency ranking of a core within its core type (Fn8000_0026).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ranking {
    core_type: u8,
    rank: u8,
}

impl Ranking {
    /// The ranking a core reports, where it reports one.
    pub const fn of(identity: Option<CoreIdentity>) -> Option<Ranking> {
        match identity {
            Some(CoreIdentity {
                core_type,
                efficiency_rank: Some(rank),
                ..
            }) => Some(Ranking { core_type, rank }),
            _ => None,
        }
    }

    /// Which of two processors to prefer for performance, between processors of equal capacity:
    /// the higher ranking, since a lower value means less power, and perhaps less performance
    /// (AMD APM Vol. 3 §E.4.24, paraphrased). Only within one core type; across types, or where
    /// either has none, there is no order.
    pub fn compare(a: Option<Ranking>, b: Option<Ranking>) -> Option<Ordering> {
        match (a, b) {
            (Some(a), Some(b)) if a.core_type == b.core_type => Some(a.rank.cmp(&b.rank)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rng, cases};

    fn some(values: &[u32]) -> Vec<Option<u32>> {
        values.iter().map(|&v| Some(v)).collect()
    }

    #[test]
    fn the_largest_is_full_and_the_rest_in_proportion() {
        let hfi = some(&[255, 128, 64, 0]);
        let (source, c) = choose(
            4,
            &Reports {
                hfi: Some(&hfi),
                ..Reports::default()
            },
        );
        assert_eq!(source, Source::Hfi);
        assert_eq!(c, [1024, 514, 257, 0]);
        // A tiny value is never rounded to "avoid".
        let cpc = some(&[100_000, 1]);
        let (source, c) = choose(
            2,
            &Reports {
                cpc: Some(&cpc),
                ..Reports::default()
            },
        );
        assert_eq!((source, c.as_slice()), (Source::Cpc, &[1024u16, 1][..]));
    }

    /// Ruling Q10: each source missing one processor falls to the next.
    #[test]
    fn a_source_missing_one_processor_falls_to_the_next() {
        let full = some(&[200, 100]);
        let gap = vec![Some(200), None];
        let hfi_gap = Reports {
            hfi: Some(&gap),
            cpc: Some(&full),
            register: Some(&full),
        };
        assert_eq!(choose(2, &hfi_gap).0, Source::Cpc);
        let cpc_gap = Reports {
            hfi: None,
            cpc: Some(&gap),
            register: Some(&full),
        };
        assert_eq!(choose(2, &cpc_gap).0, Source::Register);
        let register_gap = Reports {
            hfi: None,
            cpc: None,
            register: Some(&gap),
        };
        assert_eq!(choose(2, &register_gap), (Source::Equal, vec![FULL; 2]));
        // A list for another number of processors is no value for these.
        let short = some(&[5]);
        assert_eq!(
            choose(
                2,
                &Reports {
                    hfi: Some(&short),
                    ..Reports::default()
                }
            )
            .0,
            Source::Equal
        );
        // A Highest of 0 is no value; an HFI table all of 0 is not yet written.
        let zero = some(&[0, 100]);
        let zeros = some(&[0, 0]);
        assert_eq!(
            choose(
                2,
                &Reports {
                    hfi: Some(&zeros),
                    cpc: Some(&zero),
                    register: Some(&full),
                }
            )
            .0,
            Source::Register
        );
    }

    fn hfi_table(shape: HfiShape, rows: &[(u8, u8)], flags: u8) -> Vec<u8> {
        let mut t = vec![0u8; 8 + shape.stride() * (rows.len() + 1)];
        t[8] = flags;
        for (r, &(perf, eff)) in rows.iter().enumerate() {
            let at = 8 + (r + 1) * shape.stride();
            t[at] = perf;
            if let Some(e) = shape.place(EFFICIENCY) {
                t[at + e] = eff;
            }
        }
        t
    }

    /// S9's HFI cases: a change, an "avoid" row, force idle, efficiency 255.
    #[test]
    fn hfi_rows_and_flags() {
        let shape = HfiShape::from_cpuid(HFI, 0, 0b11).unwrap();
        assert_eq!(shape.stride(), 8);
        let t = hfi_table(shape, &[(200, 100), (0, 0), (100, 255)], 0b11);
        assert_eq!(t.len(), 16 + 3 * 8);
        let flags = hfi_flags(&t, shape).unwrap();
        assert!(flags.performance_changed && flags.performance_force_idle);
        assert!(!flags.efficiency_changed);
        let row = |r| hfi_row(&t, shape, r).unwrap();
        assert_eq!(
            row(0),
            HfiRow {
                performance: 200,
                efficiency: Some(100)
            }
        );
        assert!(row(1).avoid());
        assert!(row(2).consolidate() && !row(2).avoid());
        assert_eq!(
            hfi_row(&t, shape, 3),
            None,
            "a row past the table is refused"
        );
        // A change: a new table rescales.
        let rows: Vec<Option<u32>> = (0..3)
            .map(|r| Some(u32::from(row(r).performance)))
            .collect();
        let (_, c) = choose(
            3,
            &Reports {
                hfi: Some(&rows),
                ..Reports::default()
            },
        );
        assert_eq!(c, [1024, 0, 512]);
    }

    /// Thread Director's layout (Table 17-7): with 4 classes and 2 capabilities, a header of
    /// 8 + 8 bytes and rows of 8; with 5 classes, rows of 16. Class 0 is read.
    #[test]
    fn thread_director_rows_are_found_by_stride() {
        let four = HfiShape::from_cpuid(HFI | THREAD_DIRECTOR, 4 << 8, 0b11).unwrap();
        assert_eq!(four.stride(), 8);
        let five = HfiShape::from_cpuid(HFI | THREAD_DIRECTOR, 5 << 8, 0b11).unwrap();
        assert_eq!(five.stride(), 16);
        let mut t = vec![0u8; 8 + 16 * 3];
        t[8 + 16 * 2] = 77; // row 1, class 0, performance
        t[8 + 16 * 2 + 1] = 33; // its efficiency
        t[8 + 16 * 2 + 2] = 99; // class 1's performance
        assert_eq!(
            hfi_row(&t, five, 1),
            Some(HfiRow {
                performance: 77,
                efficiency: Some(33)
            })
        );
        // No HFI, or an HFI that does not report performance, is no shape.
        assert_eq!(HfiShape::from_cpuid(0, 0, 0b11), None);
        assert_eq!(HfiShape::from_cpuid(HFI, 0, 0b10), None);
        // Performance alone: one byte a class, no efficiency.
        let alone = HfiShape::from_cpuid(HFI, 0, 0b01).unwrap();
        assert_eq!(
            hfi_row(
                &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9],
                alone,
                0
            )
            .map(|r| r.efficiency),
            Some(None)
        );
    }

    #[test]
    fn hostile_tables_never_panic() {
        let mut rng = Rng::new(0x4846_4921);
        for case in 0..cases(20_000) {
            let shape =
                HfiShape::from_cpuid(rng.edge_u32() | HFI, rng.edge_u32(), rng.edge_u32() | 1)
                    .unwrap();
            let table: Vec<u8> = (0..rng.below(300)).map(|_| rng.byte()).collect();
            let row = rng.edge_u32() as u16;
            if let Some(r) = hfi_row(&table, shape, row) {
                let at = 8 + (usize::from(row) + 1) * shape.stride();
                assert_eq!(table[at], r.performance, "case {case}");
            }
            let _ = hfi_flags(&table, shape);
        }
    }

    fn register(space: u8, width: u8, address: u64) -> Vec<u8> {
        field_register(space, width, 0, address)
    }

    /// A Generic Register Descriptor for the `width` bits at `offset`, by quad-word access.
    fn field_register(space: u8, width: u8, offset: u8, address: u64) -> Vec<u8> {
        let mut b = vec![0x82, 12, 0, space, width, offset, 4];
        b.extend_from_slice(&address.to_le_bytes());
        b.extend_from_slice(&[0x79, 0]);
        b
    }

    /// S9's `_CPC` cases: revisions 2 to 4, an Integer and a register, Highest differing between
    /// processors, and Notify 0x85.
    #[test]
    fn cpc_highest_from_an_integer_or_a_register() {
        let none = |_: &CpcRegister| None;
        for (revision, entries) in [(2u64, 21usize), (3, 23), (4, 25)] {
            let mut package = vec![
                CpcElement::Integer(entries as u64),
                CpcElement::Integer(revision),
            ];
            package.push(CpcElement::Integer(250));
            package.resize(entries, CpcElement::Integer(0));
            assert_eq!(cpc_highest(&package, none), Ok(250), "revision {revision}");
            assert!(!cpc_rereads_highest(&package));
        }
        let msr = register(0x7F, 8, 0x771);
        let mut package = vec![
            CpcElement::Integer(25),
            CpcElement::Integer(4),
            CpcElement::Buffer(&msr),
        ];
        package.resize(25, CpcElement::Integer(0));
        // Two processors whose registers read differently: their capacities differ.
        let reads = [180u64, 120];
        let highest: Vec<Option<u32>> = reads
            .iter()
            .map(|&v| cpc_highest(&package, |r| (r.address == 0x771).then_some(v)).ok())
            .collect();
        assert_eq!(highest, [Some(180), Some(120)]);
        assert_eq!(
            choose(
                2,
                &Reports {
                    cpc: Some(&highest),
                    ..Reports::default()
                }
            )
            .1,
            [1024, 682]
        );
        // Notify 0x85: re-read the register, rescale.
        assert!(cpc_rereads_highest(&package));
        let again: Vec<Option<u32>> = [120u64, 180]
            .iter()
            .map(|&v| cpc_highest(&package, |_| Some(v)).ok())
            .collect();
        assert_eq!(
            choose(
                2,
                &Reports {
                    cpc: Some(&again),
                    ..Reports::default()
                }
            )
            .1,
            [682, 1024]
        );
        assert_eq!(cpc_highest(&package, none), Err(CpcError::Unread));
    }

    /// A register that reads whole, as a real `read` gives it: the field is taken out (R97-5).
    /// Intel's IA32_HWP_CAPABILITIES holds Highest in bits 7:0, AMD's CPPC_CAPABILITY_1
    /// HighestPerf in bits 31:24.
    #[test]
    fn a_register_field_is_taken_from_the_whole_register() {
        let hwp = field_register(0x7F, 8, 0, 0x771);
        let amd = field_register(0x7F, 8, 24, 0xC001_02B0);
        let zero = field_register(0x7F, 0, 0, 0x771);
        let mut byte = field_register(1, 8, 4, 0x80);
        byte[6] = 1;
        fn package(b: &[u8]) -> Vec<CpcElement<'_>> {
            let mut p = vec![
                CpcElement::Integer(25),
                CpcElement::Integer(4),
                CpcElement::Buffer(b),
            ];
            p.resize(25, CpcElement::Integer(0));
            p
        }
        assert_eq!(cpc_highest(&package(&hwp), |_| Some(0x0108_2A30)), Ok(0x30));
        assert_eq!(
            cpc_highest(&package(&amd), |_| Some(0x0000_00AA_7766_5544)),
            Ok(0x77)
        );
        // A field that does not fit its register is refused: width 0, or past the access size.
        assert_eq!(
            cpc_highest(&package(&zero), |_| Some(1)),
            Err(CpcError::Descriptor)
        );
        assert_eq!(
            cpc_highest(&package(&byte), |_| Some(0xFF)),
            Err(CpcError::Descriptor)
        );
    }

    #[test]
    fn malformed_cpc_packages_are_refused() {
        let read = |_: &CpcRegister| Some(1);
        let pkg = |v: Vec<CpcElement<'static>>| v;
        assert_eq!(cpc_highest(&pkg(vec![]), read), Err(CpcError::NumEntries));
        assert_eq!(
            cpc_highest(
                &pkg(vec![
                    CpcElement::Integer(4),
                    CpcElement::Integer(4),
                    CpcElement::Integer(9)
                ]),
                read
            ),
            Err(CpcError::NumEntries)
        );
        assert_eq!(
            cpc_highest(
                &pkg(vec![
                    CpcElement::Integer(3),
                    CpcElement::Other,
                    CpcElement::Integer(9)
                ]),
                read
            ),
            Err(CpcError::Revision)
        );
        for revision in [0, 1, 5] {
            assert_eq!(
                cpc_highest(
                    &pkg(vec![
                        CpcElement::Integer(3),
                        CpcElement::Integer(revision),
                        CpcElement::Integer(9)
                    ]),
                    read
                ),
                Err(CpcError::Revision),
                "{revision}"
            );
        }
        assert_eq!(
            cpc_highest(
                &pkg(vec![
                    CpcElement::Integer(3),
                    CpcElement::Integer(4),
                    CpcElement::Other
                ]),
                read
            ),
            Err(CpcError::Highest)
        );
        assert_eq!(
            cpc_highest(
                &pkg(vec![
                    CpcElement::Integer(3),
                    CpcElement::Integer(4),
                    CpcElement::Integer(1 << 32)
                ]),
                read
            ),
            Err(CpcError::Value)
        );
        assert_eq!(
            cpc_highest(
                &pkg(vec![
                    CpcElement::Integer(3),
                    CpcElement::Integer(4),
                    CpcElement::Integer(0)
                ]),
                read
            ),
            Err(CpcError::Value)
        );
        let null = register(0, 0, 0);
        assert_eq!(
            cpc_highest(
                &[
                    CpcElement::Integer(3),
                    CpcElement::Integer(4),
                    CpcElement::Buffer(&null)
                ],
                read
            ),
            Err(CpcError::NullRegister)
        );
        let short = [0x82u8, 12, 0];
        assert_eq!(
            cpc_highest(
                &[
                    CpcElement::Integer(3),
                    CpcElement::Integer(4),
                    CpcElement::Buffer(&short)
                ],
                read
            ),
            Err(CpcError::Descriptor)
        );
        let mut wrong = register(1, 8, 0x80);
        wrong[0] = 0x86;
        assert_eq!(generic_register(&wrong), Err(CpcError::Descriptor));
        let mut rng = Rng::new(0x5F43_5043);
        for _ in 0..cases(10_000) {
            let b: Vec<u8> = (0..rng.below(24)).map(|_| rng.byte()).collect();
            let _ = generic_register(&b);
        }
    }

    #[test]
    fn the_registers_highest_fields() {
        assert_eq!(hwp_highest(0x0102_03FE), 0xFE);
        assert_eq!(amd_cppc_highest(0x0000_00AA_7766_5544), 0x77);
        assert_eq!(amd_cppc_highest(0xFF_D1_00_00_00), 0xD1);
    }

    /// S9's Fn8000_0026 cases: core types and rankings. The ranking orders within a type and never
    /// across. (The heterogeneous flag is `topology`'s `hybrid`, which no capacity reads.)
    #[test]
    fn rankings_order_within_a_core_type_only() {
        let id = |core_type, rank| {
            Some(CoreIdentity {
                core_type,
                native_model: 0,
                efficiency_rank: rank,
            })
        };
        let a = Ranking::of(id(0, Some(3)));
        let b = Ranking::of(id(0, Some(7)));
        let c = Ranking::of(id(1, Some(9)));
        assert_eq!(Ranking::compare(a, b), Some(Ordering::Less));
        assert_eq!(Ranking::compare(b, a), Some(Ordering::Greater));
        assert_eq!(Ranking::compare(a, c), None);
        assert_eq!(Ranking::compare(a, Ranking::of(id(0, None))), None);
        assert_eq!(Ranking::of(None), None);
    }

    /// S27-10. No capacity can come from a core type: [`choose`] takes none. The one reader of a
    /// core type, [`Ranking::compare`], only says which processors a ranking may order: never two
    /// of different types, and within one type only by their rankings, whatever the codes are.
    #[test]
    fn a_core_type_only_scopes_a_ranking() {
        let mut rng = Rng::new(0x5332_3731);
        for case in 0..cases(4000) {
            let id = |rng: &mut Rng| {
                Some(CoreIdentity {
                    core_type: rng.below(4) as u8,
                    native_model: rng.below(16) as u32,
                    efficiency_rank: (rng.below(5) != 0).then(|| rng.byte()),
                })
            };
            let (a, b) = (id(&mut rng), id(&mut rng));
            let order = Ranking::compare(Ranking::of(a), Ranking::of(b));
            let (a, b) = (a.unwrap(), b.unwrap());
            match (a.efficiency_rank, b.efficiency_rank) {
                (Some(x), Some(y)) if a.core_type == b.core_type => {
                    assert_eq!(order, Some(x.cmp(&y)), "case {case}");
                }
                _ => assert_eq!(order, None, "case {case}"),
            }
        }
    }
}
