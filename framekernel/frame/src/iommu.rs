//! DMA and interrupt remapping (CLAUDE.md §32 Intel VT-d, AMD IOMMU and interrupt
//! remapping, §58 DMA protection; ADR-0007, Proposed): the entry points drivers use, and the
//! Intel VT-d driver (research ledger `docs/research/2026-11-08-vt-d-iommu.md`). The AMD-Vi
//! driver is the child module [`amd`] (`docs/research/2026-11-08-amd-vi-iommu.md`); every
//! entry point below goes there when the firmware described AMD IOMMUs (an IVRS) instead
//! of VT-d units (a DMAR), so drivers do not know which IOMMU they run behind.
//!
//! At boot, after PCIe enumeration and before any driver binds:
//! 1. the ACPI DMAR names the remapping units (`crate::dmar`, checked whole);
//! 2. each unit's registers are mapped uncached and its capabilities read and checked;
//! 3. queued invalidation is turned on; an empty root table is set and the context and
//!    IOTLB caches are invalidated; where the unit and the platform support interrupt
//!    remapping, an interrupt remapping table is set (32-bit x2APIC destinations when the
//!    unit has extended interrupt mode), its cache invalidated and remapping turned on, with
//!    compatibility-format interrupts left blocked;
//! 4. translation is turned on. No context entry is present yet, so every device's DMA
//!    faults until its driver attaches it (ADR-0007 D2).
//!
//! A driver then attaches its device ([`attach`]): the device gets a domain of its own — a
//! domain ID, second-level tables and an IOVA window — and a context entry. Each DMA page the
//! driver allocates is mapped there ([`map`]) at an IOVA from [1 GiB, 2 GiB), never at its
//! physical address. Unmapping ([`unmap`]) invalidates the IOTLB — page-selective, draining
//! reads and writes where the unit can — and waits for it before returning. MSI-X messages
//! are routed through IRTEs ([`msi_message`]): the device's table entry carries only a
//! remappable-format handle; the vector, the 32-bit x2APIC destination and the check of the
//! device's source ID live in the IRTE. An I/O APIC pin is routed the same way
//! ([`route_ioapic_pin`]): its redirection entry, in remappable format, carries the handle,
//! and the IRTE checks the source ID the DMAR names for that I/O APIC and has the pin's
//! trigger mode.
//!
//! Faults are read from the fault-recording registers ([`take_faults`]); fault events stay
//! masked (ADR-0007 D9). Every remapping structure is a zeroed kernel page kept for ever, and
//! when the unit's walks do not snoop the caches (ECAP.C clear) every line written is flushed
//! before the hardware can need it (D7). The `unsafe` here is volatile access to those pages
//! and CLFLUSH; each has a SAFETY note.
//!
//! Sound against safe callers (ADR-0011 N6, N12: IOMMU programming is the framework's): every
//! remapping structure is private to this module, a domain's tables map only [`DmaFrame`]s
//! (kept for ever, and never a Rust object), so no call in any order, with any argument, can give
//! a device a reference to memory safe code holds; [`init`] runs once. A wrong interrupt route
//! is a logic error of its caller: it can only deliver a vector the IDT has a gate for.

use alloc::alloc::{Layout, alloc_zeroed};
use alloc::vec::Vec;
use core::arch::asm;
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering, fence};

use crate::addr::{Iova, PhysAddr};
use crate::amdvi;
use crate::bitmap::Bitmap;
use crate::dma::DmaFrame;
use crate::dmar::{self, BridgeBuses, Dmar, IoApicScopeError, Requester};
use crate::mmio::MmioRegion;
use crate::once::SetOnce;
use crate::pci::{self, Address};
use crate::sync::SpinLock;
use crate::vtd::{
    self, Access, Capability, ContextEntry, Descriptor, Drain, ExtCapability, FaultRecord,
    IovaWindow, Irte, MsiMessage, SourceCheck, TableMemory, fsts, gcmd, reg,
};

mod amd;

/// A 4 KiB page.
const PAGE: u64 = vtd::PAGE;
/// Entries in each unit's interrupt remapping table (one page of 16-byte entries).
const IRT_ENTRIES: u32 = 256;
/// How long a register handshake or an invalidation may take.
const TIMEOUT_NS: u64 = 100_000_000;
/// Units the kernel drives.
const MAX_UNITS: usize = 8;

/// A zeroed 4 KiB kernel page, kept for ever (the remapping hardware may read it at any
/// time): its direct-map address and its physical address.
#[derive(Clone, Copy, Debug)]
struct Page {
    virt: u64,
    phys: u64,
}

impl Page {
    /// A new page; flushed from the caches when the unit's walks are not coherent, so the
    /// hardware reads zeros rather than whatever the memory held before.
    fn new(coherent: bool) -> Option<Self> {
        let layout = Layout::from_size_align(PAGE as usize, PAGE as usize).ok()?;
        // SAFETY: `layout` has a non-zero size. The page is never freed: the remapping
        // hardware may read it at any time.
        let p = unsafe { alloc_zeroed(layout) };
        if p.is_null() {
            return None;
        }
        let virt = p.expose_provenance() as u64;
        if !coherent {
            flush(virt, PAGE);
        }
        Some(Self {
            virt,
            phys: crate::phys::to_phys(virt),
        })
    }

    fn read64(&self, offset: u64) -> u64 {
        if offset + 8 > PAGE {
            return 0;
        }
        let p = ptr::with_exposed_provenance::<u64>((self.virt + offset) as usize);
        // SAFETY: inside this live, never-freed page, 8-byte aligned (callers use multiples
        // of 8). The hardware may read it concurrently, so the access is volatile and no
        // reference is formed.
        unsafe { p.read_volatile() }
    }

    fn write64(&self, offset: u64, value: u64) {
        if offset + 8 > PAGE {
            return;
        }
        let p = ptr::with_exposed_provenance_mut::<u64>((self.virt + offset) as usize);
        // SAFETY: as for `read64`. The page holds remapping structures only this module
        // writes.
        unsafe { p.write_volatile(value) };
    }

    fn read32(&self, offset: u64) -> u32 {
        if offset + 4 > PAGE {
            return 0;
        }
        let p = ptr::with_exposed_provenance::<u32>((self.virt + offset) as usize);
        // SAFETY: as for `read64`, 4-byte aligned. The unit writes a wait descriptor's
        // status here.
        unsafe { p.read_volatile() }
    }

    fn write32(&self, offset: u64, value: u32) {
        if offset + 4 > PAGE {
            return;
        }
        let p = ptr::with_exposed_provenance_mut::<u32>((self.virt + offset) as usize);
        // SAFETY: as for `read32`.
        unsafe { p.write_volatile(value) };
    }
}

/// Writes back and invalidates the cache lines of `[virt, virt + len)`, then fences, so
/// remapping hardware that does not snoop the caches reads what the processor wrote.
/// `virt` must be a live kernel page from [`Page::new`].
fn flush(virt: u64, len: u64) {
    let mut line = virt & !63;
    while line < virt + len {
        // SAFETY: `line` lies in a live, mapped kernel page (the caller's contract), and
        // CLFLUSH only writes the line back and drops it from the caches. No `preserves_flags`:
        // Intel's entry for CLFLUSH does not say what it does to the flags
        // (docs/research/2026-11-10-asm-flags.md).
        unsafe { asm!("clflush [{}]", in(reg) line, options(nostack)) };
        line += 64;
    }
    // MFENCE orders the flushes before every later store (the register write that lets
    // the hardware use the structure).
    fence(Ordering::SeqCst);
}

/// A unit's register set, mapped uncached.
#[derive(Clone, Copy, Debug)]
struct Regs {
    region: MmioRegion,
}

impl Regs {
    fn read32(&self, offset: u64) -> u32 {
        self.region.read32(offset as usize)
    }

    fn write32(&self, offset: u64, value: u32) {
        self.region.write32(offset as usize, value);
    }

    fn read64(&self, offset: u64) -> u64 {
        self.region.read64(offset as usize)
    }

    fn write64(&self, offset: u64, value: u64) {
        self.region.write64(offset as usize, value);
    }
}

/// A unit's interrupt remapping table.
struct Irt {
    table: Page,
    /// Entries handed out (an entry can be in use while not present: see
    /// [`reserve_blocked_interrupt`]).
    used: Bitmap,
    /// EIME: destinations are 32-bit x2APIC IDs.
    x2apic: bool,
}

/// One running remapping unit.
struct Unit {
    /// Physical base of its registers (its identity in the DMAR).
    base: u64,
    regs: Regs,
    cap: Capability,
    ecap: ExtCapability,
    /// Second-level table depth for its domains.
    levels: u8,
    root: Page,
    /// The context table of each bus, made on demand.
    contexts: Vec<Option<Page>>,
    /// The invalidation queue and the next free descriptor.
    queue: Page,
    tail: usize,
    /// Where wait descriptors write their status, and the last status used.
    status: Page,
    sequence: u32,
    irt: Option<Irt>,
    next_domain: u32,
}

/// One device's domain.
#[derive(Clone, Copy, Debug)]
struct DomainState {
    unit: usize,
    id: u16,
    requester: Requester,
    /// Physical address of its top-level second-level table.
    top: u64,
    window: IovaWindow,
    /// Pages mapped now, and pages ever mapped.
    mapped: u64,
    mapped_ever: u64,
    /// Table pages (the top one included).
    tables: u64,
}

/// A device's DMA domain, as drivers hold it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Domain(usize);

struct Iommu {
    dmar: Dmar<'static>,
    units: Vec<Unit>,
    domains: Vec<DomainState>,
    /// Every unit translates.
    translating: bool,
    /// Every unit remaps interrupts.
    remapping: bool,
}

static IOMMU: SpinLock<Option<Iommu>> = SpinLock::new(None);
/// Whether [`init`] has run.
static STARTED: AtomicBool = AtomicBool::new(false);
/// What [`init`] established about the platform's IOMMUs.
static STATUS: SpinLock<pci::IommuStatus> = SpinLock::new(pci::IommuStatus::NotRun);

/// What `firmware::acpi_table` says when the XSDT does not list a table: the one failure that
/// means the table is really absent, not unreadable.
const NOT_LISTED: &str = "table not listed in the XSDT";

/// The table with signature `sig`, or `None` if the firmware does not list one: unlike
/// `firmware::acpi_table`, this tells an absent table from one that cannot be read.
pub fn acpi_table_if_listed(
    rsdp: u64,
    sig: &[u8; 4],
) -> Result<Option<&'static [u8]>, &'static str> {
    match crate::firmware::acpi_table(rsdp, sig) {
        Ok(t) => Ok(Some(t)),
        Err(NOT_LISTED) => Ok(None),
        Err(why) => Err(why),
    }
}

/// Reads the 32-bit PCI configuration register at byte offset `register` of the function at
/// the [`Address`]. The configuration space belongs to the PCIe layer above: what it answers
/// only decides which IOMMU unit and which requester an attachment names, never what memory a
/// domain can reach (a domain maps [`DmaFrame`]s and nothing else).
pub type ConfigRead = fn(Address, u16) -> Option<u32>;

/// How to read configuration space, given to [`init`].
static CONFIG_READ: SetOnce<ConfigRead> = SetOnce::new();

fn read_config32(at: Address, register: u16) -> Option<u32> {
    CONFIG_READ.get().and_then(|read| read(at, register))
}

/// Bridge bus numbers from configuration space, for the DMAR's device-scope paths.
struct Config;

impl BridgeBuses for Config {
    fn buses(&mut self, b: Requester) -> Option<(u8, u8)> {
        if b.segment != 0 {
            return None;
        }
        let at = Address {
            bus: b.bus,
            device: b.device,
            function: b.function,
        };
        let id = read_config32(at, pci::register::ID)?;
        let header = read_config32(at, pci::register::HEADER)?;
        if id & 0xFFFF == 0xFFFF || (header >> 16) & 0x7F != 1 {
            return None;
        }
        // Type 1 header: primary, secondary and subordinate bus numbers at 18h.
        let buses = read_config32(at, 0x18)?;
        Some((((buses >> 8) & 0xFF) as u8, ((buses >> 16) & 0xFF) as u8))
    }
}

fn requester(at: Address) -> Requester {
    Requester {
        segment: 0,
        bus: at.bus,
        device: at.device,
        function: at.function,
    }
}

/// Spins until `done()` holds or [`TIMEOUT_NS`] passes; returns whether it held.
fn poll(mut done: impl FnMut() -> bool) -> bool {
    let deadline = crate::monotonic::monotonic_nanos() + TIMEOUT_NS;
    loop {
        if done() {
            return true;
        }
        if crate::monotonic::monotonic_nanos() > deadline {
            return done();
        }
        core::hint::spin_loop();
    }
}

/// The second-level tables of one unit's domains, through the direct map.
struct Tables {
    coherent: bool,
    made: u64,
}

impl TableMemory for Tables {
    fn read(&mut self, table: u64, index: usize) -> u64 {
        if index >= vtd::ENTRIES {
            return 0;
        }
        let p = ptr::with_exposed_provenance::<u64>(
            (crate::phys::to_virt(table) + 8 * index as u64) as usize,
        );
        // SAFETY: `table` is a table page this module made (`new_table`: a live kernel
        // page, reached through the direct map), and `index` is below 512, so the entry
        // lies inside it, 8-byte aligned. The hardware may read it concurrently: volatile.
        unsafe { p.read_volatile() }
    }

    fn write(&mut self, table: u64, index: usize, value: u64) {
        if index >= vtd::ENTRIES {
            return;
        }
        let virt = crate::phys::to_virt(table) + 8 * index as u64;
        let p = ptr::with_exposed_provenance_mut::<u64>(virt as usize);
        // SAFETY: as for `read`; the entry belongs to this module's tables.
        unsafe { p.write_volatile(value) };
        if !self.coherent {
            flush(virt, 8);
        }
    }

    fn new_table(&mut self) -> Option<u64> {
        let page = Page::new(self.coherent)?;
        self.made += 1;
        Some(page.phys)
    }
}

impl Unit {
    /// Turns the unit off again, one field per GCMD write (chapter 11): translation, then
    /// interrupt remapping, then queued invalidation, each only once the one before is off.
    fn shut_down(&self) {
        let _ = self.command(0, gcmd::TE, gcmd::TE, false);
        let _ = self.command(0, gcmd::IRE, gcmd::IRE, false);
        let _ = self.command(0, gcmd::QIE, gcmd::QIE, false);
    }

    /// Issues a global command and waits for its status bit to read `want`.
    fn command(&self, set: u32, clear: u32, status: u32, want: bool) -> Result<(), &'static str> {
        let gsts = self.regs.read32(reg::GSTS);
        self.regs.write32(reg::GCMD, vtd::gcmd(gsts, set, clear));
        if poll(|| (self.regs.read32(reg::GSTS) & status != 0) == want) {
            Ok(())
        } else {
            Err("a global command was not acknowledged")
        }
    }

    /// After structures changed: flushes the write buffer when the unit needs it (RWBF).
    fn write_buffer_flush(&self) -> Result<(), &'static str> {
        if self.cap.write_buffer_flush {
            self.command(gcmd::WBF, 0, gcmd::WBF, false)?;
        }
        Ok(())
    }

    /// Queues `descriptors` and a wait descriptor, then waits (bounded) until the unit has
    /// written the wait's status: every descriptor before it has completed.
    fn invalidate(&mut self, descriptors: &[Descriptor]) -> Result<(), &'static str> {
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let sequence = self.sequence;
        self.status.write32(0, 0);
        let wait = Descriptor::Wait {
            address: self.status.phys,
            status: sequence,
            fence: true,
        };
        let entries = vtd::queue_entries(0);
        for d in descriptors.iter().chain(core::iter::once(&wait)) {
            let [lo, hi] = d.encode().ok_or("an invalidation does not encode")?;
            let at = 16 * self.tail as u64;
            self.queue.write64(at, lo);
            self.queue.write64(at + 8, hi);
            if !self.ecap.coherent {
                flush(self.queue.virt + at, 16);
            }
            self.tail = (self.tail + 1) % entries;
        }
        fence(Ordering::SeqCst);
        self.regs.write64(reg::IQT, vtd::queue_offset(self.tail));
        let mut error = false;
        let done = poll(|| {
            error = self.regs.read32(reg::FSTS) & fsts::IQE != 0;
            error || self.status.read32(0) == sequence
        });
        if error {
            Err("the invalidation queue reported an error")
        } else if !done {
            Err("an invalidation did not complete")
        } else {
            Ok(())
        }
    }

    /// What an IOTLB invalidation may drain on this unit.
    fn drain(&self) -> Drain {
        Drain {
            reads: self.cap.drain_reads,
            writes: self.cap.drain_writes,
        }
    }

    /// Reads and clears every recorded fault, and the overflow bit; returns the faults and
    /// whether any were lost.
    fn take_faults(&mut self) -> (Vec<FaultRecord>, bool) {
        let mut out = Vec::new();
        for i in 0..u64::from(self.cap.fault_records) {
            let at = self.cap.fault_offset + 16 * i;
            let (lo, hi) = (self.regs.read64(at), self.regs.read64(at + 8));
            if let Some(f) = vtd::fault_record(lo, hi) {
                out.push(f);
                self.regs.write64(at + 8, vtd::FRCD_FAULT);
            }
        }
        let status = self.regs.read32(reg::FSTS);
        let overflow = status & fsts::PFO != 0;
        if overflow {
            self.regs.write32(reg::FSTS, fsts::PFO);
        }
        (out, overflow)
    }
}

/// What [`init`] found and did, for the report.
struct Brought {
    unit: Unit,
    version: (u8, u8),
    cap_raw: u64,
    ecap_raw: u64,
    stale: usize,
    gsts: u32,
}

/// Brings up the unit `u`: checks its registers, turns on queued invalidation, sets an empty
/// root table, sets up interrupt remapping when `remap`, and turns on translation. A unit that
/// fails after any of that began is turned off again before the error returns.
fn bring_up(u: &dmar::Unit<'static>, remap: bool) -> Result<Brought, &'static str> {
    let size = u64::from(u.register_pages) * PAGE;
    let region = crate::vm::map_mmio(PhysAddr::new(u.register_base), size)
        .map_err(|_| "cannot map its registers")?;
    let regs = Regs { region };
    let version = vtd::version(regs.read32(reg::VER));
    let cap_raw = regs.read64(reg::CAP);
    let ecap_raw = regs.read64(reg::ECAP);
    let cap = Capability::decode(cap_raw);
    let ecap = ExtCapability::decode(ecap_raw);
    if cap.fault_offset + 16 * u64::from(cap.fault_records) > size
        || ecap.iotlb_offset + 16 > size
        || cap.fault_offset < 0x100
    {
        return Err("its fault-recording or IOTLB registers lie outside its register set");
    }
    let levels = cap
        .table_levels()
        .ok_or("it supports neither 3- nor 4-level second-level tables")?;
    if cap.domain_bits == 0 {
        return Err("its domain-ID width is reserved");
    }
    if !ecap.queued_invalidation {
        return Err(
            "it has no queued invalidation (register-based invalidation is not implemented)",
        );
    }
    let before = regs.read32(reg::GSTS);
    if before & (gcmd::TE | gcmd::QIE | gcmd::IRE) != 0 {
        return Err("the firmware left it enabled (taking a unit over is not implemented)");
    }
    let coherent = ecap.coherent;
    let page = || Page::new(coherent).ok_or("no memory for its structures");
    let mut unit = Unit {
        base: u.register_base,
        regs,
        cap,
        ecap,
        levels,
        root: page()?,
        contexts: alloc::vec![None; 256],
        queue: page()?,
        tail: 0,
        status: page()?,
        sequence: 0,
        irt: None,
        next_domain: 1,
    };
    // Stale faults from before the kernel (the firmware's time) are dropped.
    let (stale, _) = unit.take_faults();
    unit.regs.write32(reg::FSTS, fsts::CLEAR);
    // Fault events stay masked (ADR-0007 I9), whatever the firmware left.
    let fectl = unit.regs.read32(reg::FECTL);
    unit.regs.write32(reg::FECTL, fectl | vtd::FECTL_IM);
    if let Err(why) = enable(&mut unit, remap) {
        unit.shut_down();
        return Err(why);
    }
    let gsts = unit.regs.read32(reg::GSTS);
    Ok(Brought {
        unit,
        version,
        cap_raw,
        ecap_raw,
        stale: stale.len(),
        gsts,
    })
}

/// Turns on queued invalidation, the empty root table, interrupt remapping when `remap`, and
/// translation, in that order (one field per GCMD write); the caller shuts the unit down on
/// failure.
fn enable(unit: &mut Unit, remap: bool) -> Result<(), &'static str> {
    // Queued invalidation: the tail at 0, one page of 128-bit descriptors.
    unit.regs.write64(reg::IQT, 0);
    unit.regs.write64(
        reg::IQA,
        vtd::iqa(unit.queue.phys, 0).ok_or("the invalidation queue is misplaced")?,
    );
    unit.command(gcmd::QIE, 0, gcmd::QIE, true)?;
    // The root table, legacy mode (RTADDR.TTM 00), all entries not present; then both
    // translation caches invalidated, as the specification asks after a new root.
    unit.regs.write64(reg::RTADDR, unit.root.phys);
    unit.command(gcmd::SRTP, 0, gcmd::SRTP, true)?;
    let drain = unit.drain();
    unit.invalidate(&[Descriptor::ContextGlobal, Descriptor::IotlbGlobal { drain }])?;
    // Interrupt remapping: the table, its cache invalidated, then remapping on. CFI stays
    // clear: compatibility-format interrupts are blocked.
    if remap && unit.ecap.interrupt_remapping {
        let x2apic = unit.ecap.extended_interrupt_mode;
        let table = Page::new(unit.ecap.coherent).ok_or("no memory for its structures")?;
        unit.regs.write64(
            reg::IRTA,
            vtd::irta(table.phys, IRT_ENTRIES, x2apic).ok_or("the interrupt table is misplaced")?,
        );
        unit.command(gcmd::SIRTP, 0, gcmd::SIRTP, true)?;
        unit.invalidate(&[Descriptor::IecGlobal])?;
        // One field per GCMD write (chapter 11): compatibility format off first, if the
        // firmware had allowed it, then remapping on.
        if unit.regs.read32(reg::GSTS) & gcmd::CFI != 0 {
            unit.command(0, gcmd::CFI, gcmd::CFI, false)?;
        }
        unit.command(gcmd::IRE, 0, gcmd::IRE, true)?;
        unit.irt = Some(Irt {
            table,
            used: Bitmap::new(IRT_ENTRIES as usize),
            x2apic,
        });
    }
    // Translation on: from here, a device without a context entry cannot reach memory.
    unit.write_buffer_flush()?;
    unit.command(gcmd::TE, 0, gcmd::TE, true)?;
    Ok(())
}

/// Finds the IOMMUs through the ACPI DMAR or IVRS and brings each up (translation and, where
/// supported, interrupt remapping on), and records the outcome ([`status`]): drivers bind only
/// if every IOMMU runs, or if the platform has none at all. Returns `false` only for a failure;
/// a platform with neither table is reported and left untranslated (the owner's decision,
/// sheet item 9: internal devices may still bind). A table that exists but cannot be read is a
/// failure, not an absence.
pub fn init(com1: &mut dyn Write, rsdp: u64, config: ConfigRead) -> bool {
    // Once: the hardware is brought up by the first call, and a later one must not disturb it.
    if STARTED.swap(true, Ordering::AcqRel) {
        return false;
    }
    let _ = CONFIG_READ.set(config);
    let status = bring_up_all(com1, rsdp);
    crate::cpu::without_interrupts(|| *STATUS.lock() = status);
    status != pci::IommuStatus::Failed
}

/// What the kernel knows about the platform's IOMMUs.
pub fn status() -> pci::IommuStatus {
    crate::cpu::without_interrupts(|| *STATUS.lock())
}

/// The physical ranges of every IOMMU's registers the firmware names, whether or not it came
/// up: no BAR may overlap them. `Err` if a table that exists cannot be read.
pub fn register_ranges(rsdp: u64) -> Result<Vec<(u64, u64)>, &'static str> {
    let mut ranges = Vec::new();
    if let Some(table) = acpi_table_if_listed(rsdp, b"DMAR")? {
        let dmar = dmar::parse(table).map_err(|_| "the DMAR is invalid")?;
        ranges.extend(
            dmar.units()
                .map(|u| (u.register_base, u64::from(u.register_pages) * PAGE)),
        );
    }
    if let Some(table) = acpi_table_if_listed(rsdp, b"IVRS")? {
        ranges.extend(amd::register_ranges(table)?);
    }
    Ok(ranges)
}

/// The VT-d or AMD-Vi bring-up itself, and what it found.
fn bring_up_all(com1: &mut dyn Write, rsdp: u64) -> pci::IommuStatus {
    use pci::IommuStatus::{Absent, Failed, Translating};
    let failed = |com1: &mut dyn Write, why: &str| {
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: the firmware's IOMMU tables cannot be read ({why}): IOMMU failed"
        );
        Failed
    };
    let table = match acpi_table_if_listed(rsdp, b"DMAR") {
        Ok(Some(t)) => t,
        Err(why) => return failed(com1, why),
        // No VT-d units: AMD IOMMUs, if the firmware describes any.
        Ok(None) => {
            return match acpi_table_if_listed(rsdp, b"IVRS") {
                Ok(Some(ivrs)) => {
                    if amd::init(com1, ivrs) {
                        Translating
                    } else {
                        Failed
                    }
                }
                Err(why) => failed(com1, why),
                Ok(None) => {
                    let _ = writeln!(
                        com1,
                        "origin-kernel: IOMMU: no DMAR or IVRS ({NOT_LISTED}): device DMA is not confined and interrupts are not remapped"
                    );
                    Absent
                }
            };
        }
    };
    let dmar = match dmar::parse(table) {
        Ok(d) => d,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: IOMMU: the DMAR is invalid ({e:?}): IOMMU failed"
            );
            return Failed;
        }
    };
    let units: Vec<_> = dmar.units().collect();
    let _ = writeln!(
        com1,
        "origin-kernel: IOMMU: DMAR: host address width {} bits, interrupt remapping {}, x2APIC opt-out {}, DMA protection opt-in {}, DMA remapping opt-out {}; {} remapping unit(s), {} reserved memory region(s)",
        dmar.host_address_width,
        dmar.interrupt_remapping(),
        dmar.x2apic_opt_out(),
        dmar.dma_control_opt_in(),
        dmar.dma_remap_opt_out(),
        units.len(),
        dmar.reserved_memory().count()
    );
    if units.is_empty() || units.len() > MAX_UNITS {
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: {} remapping units (1-{MAX_UNITS} supported): IOMMU failed",
            units.len()
        );
        return Failed;
    }
    for r in dmar.reserved_memory() {
        // Identity-mapping a reserved region into its devices' domains is not implemented
        // yet (ADR-0007): such a device is refused at `attach`, so its DMA stays blocked
        // rather than reaching memory unchecked. QEMU's q35 reports none.
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: reserved memory {:#x}-{:#x} for {} device scope(s): not identity-mapped yet, so those devices cannot be attached",
            r.base,
            r.limit,
            r.scopes().count()
        );
    }
    let mut running = Vec::with_capacity(units.len());
    for (i, u) in units.iter().enumerate() {
        let pci = u
            .scopes()
            .filter(|s| s.kind == dmar::ScopeKind::Endpoint || s.kind == dmar::ScopeKind::Bridge)
            .count();
        let ioapics: Vec<_> = u
            .scopes()
            .filter(|s| s.kind == dmar::ScopeKind::IoApic)
            .filter_map(|s| {
                s.resolve(u.segment, &mut Config)
                    .map(|r| (s.enumeration_id, r.source_id()))
            })
            .collect();
        let _ = write!(
            com1,
            "origin-kernel: IOMMU: unit {i} at {:#x} (segment {}, {} KiB of registers): {}{} PCI scope(s)",
            u.register_base,
            u.segment,
            u.register_pages * 4,
            if u.include_pci_all {
                "every PCI device not listed elsewhere, "
            } else {
                ""
            },
            pci
        );
        for (id, sid) in &ioapics {
            let _ = write!(com1, ", I/O APIC {id} (source ID {sid:#06x})");
        }
        let _ = writeln!(com1);
        match bring_up(u, dmar.interrupt_remapping()) {
            Ok(b) => {
                let c = b.unit.cap;
                let e = b.unit.ecap;
                let _ = writeln!(
                    com1,
                    "origin-kernel: IOMMU: unit {i}: VT-d {}.{}; CAP {:#x}: {}-bit domain IDs, {}-level tables ({}-bit), {} fault record(s) at {:#x}, page-selective invalidation {}, drain reads/writes {}/{}, caching mode {}, write-buffer flush {}; ECAP {:#x}: queued invalidation {}, interrupt remapping {}, extended interrupt mode {}, coherent walks {}",
                    b.version.0,
                    b.version.1,
                    b.cap_raw,
                    c.domain_bits,
                    b.unit.levels,
                    12 + 9 * u32::from(b.unit.levels),
                    c.fault_records,
                    c.fault_offset,
                    c.page_invalidation,
                    c.drain_reads,
                    c.drain_writes,
                    c.caching_mode,
                    c.write_buffer_flush,
                    b.ecap_raw,
                    e.queued_invalidation,
                    e.interrupt_remapping,
                    e.extended_interrupt_mode,
                    e.coherent
                );
                let remapping = b.unit.irt.as_ref().map(|t| t.x2apic);
                let _ = writeln!(
                    com1,
                    "origin-kernel: IOMMU: unit {i}: queued invalidation on (256 descriptors); empty root table at {:#x}, context and IOTLB caches invalidated; {}; translation on (GSTS {:#x}){}: every device's DMA faults until its driver attaches it; {} stale fault(s) cleared",
                    b.unit.root.phys,
                    match remapping {
                        Some(true) =>
                            "interrupt remapping on (256 entries, 32-bit x2APIC destinations, compatibility format blocked)",
                        Some(false) =>
                            "interrupt remapping on (256 entries, 8-bit xAPIC destinations, compatibility format blocked)",
                        None => "interrupt remapping off",
                    },
                    b.gsts,
                    if b.gsts & gcmd::TE != 0 {
                        ""
                    } else {
                        ": IOMMU failed"
                    },
                    b.stale
                );
                running.push(b.unit);
            }
            Err(why) => {
                // No driver will bind now (`dma_path`), and the units already running would
                // block every firmware-owned device with nobody to attach it: turn them off
                // again (one field per write).
                for u in &running {
                    u.shut_down();
                }
                let _ = writeln!(com1, "origin-kernel: IOMMU: unit {i}: {why}: IOMMU failed");
                return Failed;
            }
        }
    }
    let translating = running
        .iter()
        .all(|u| u.regs.read32(reg::GSTS) & gcmd::TE != 0);
    let remapping = running.iter().all(|u| u.irt.is_some());
    crate::cpu::without_interrupts(|| {
        *IOMMU.lock() = Some(Iommu {
            dmar,
            units: running,
            domains: Vec::new(),
            translating,
            remapping,
        });
    });
    if translating { Translating } else { Failed }
}

/// Whether DMA remapping is on (drivers must then attach their devices).
pub fn translating() -> bool {
    if amd::active() {
        return amd::translating();
    }
    crate::cpu::without_interrupts(|| IOMMU.lock().as_ref().is_some_and(|s| s.translating))
}

/// Whether domain `d` is the one the PCI function at `at` was attached to.
pub fn attached(d: Domain, at: Address) -> bool {
    if amd::active() {
        return amd::attached(d, at);
    }
    let r = requester(at);
    crate::cpu::without_interrupts(|| {
        IOMMU
            .lock()
            .as_ref()
            .is_some_and(|s| s.domains.get(d.0).is_some_and(|dom| dom.requester == r))
    })
}

/// The PCI functions that are the IOMMUs themselves, from the firmware's IVRS: an AMD IOMMU
/// is a PCI function, a VT-d unit is not. A domain cannot confine the hardware that enforces
/// domains, and AMD's specification (48882) does not say what such a function's Bus Master
/// Enable does, so `pcie::quiesce` leaves these alone. The list comes from firmware, never
/// from a device's class code: a device can claim to be any class.
pub fn own_functions(rsdp: u64) -> Vec<Address> {
    amd::own_functions(rsdp)
}

impl Iommu {
    /// The running unit that translates `r`.
    fn unit_of(&self, r: Requester) -> Result<usize, &'static str> {
        let base = self
            .dmar
            .unit_for(r, &mut Config)
            .ok_or("no remapping unit translates this device")?
            .register_base;
        self.units
            .iter()
            .position(|u| u.base == base)
            .ok_or("its remapping unit is not running")
    }

    fn attach(&mut self, r: Requester) -> Result<Domain, &'static str> {
        if let Some(i) = self.domains.iter().position(|d| d.requester == r) {
            return Ok(Domain(i));
        }
        if self
            .dmar
            .reserved_memory()
            .any(|m| m.segment == r.segment && m.covers(r, &mut Config))
        {
            return Err("it needs reserved memory identity-mapped, which is not implemented");
        }
        let ui = self.unit_of(r)?;
        let unit = self.units.get_mut(ui).ok_or("no unit")?;
        let coherent = unit.ecap.coherent;
        if unit.next_domain >= 1 << unit.cap.domain_bits {
            return Err("its unit has no free domain ID");
        }
        let id = unit.next_domain as u16;
        // Consumed before any entry names it: an attach that fails below may have left a
        // context entry behind, and that domain ID must never go to another device.
        unit.next_domain += 1;
        let top = Page::new(coherent).ok_or("no memory for a page table")?;
        let bus = usize::from(r.bus);
        let context = match unit.contexts.get(bus).copied().flatten() {
            Some(c) => c,
            None => {
                let c = Page::new(coherent).ok_or("no memory for a context table")?;
                let [lo, hi] = vtd::root_entry(c.phys).ok_or("a context table is misplaced")?;
                let at = 16 * bus as u64;
                unit.root.write64(at + 8, hi);
                fence(Ordering::SeqCst);
                unit.root.write64(at, lo);
                if !coherent {
                    flush(unit.root.virt + at, 16);
                }
                if let Some(slot) = unit.contexts.get_mut(bus) {
                    *slot = Some(c);
                }
                c
            }
        };
        let [lo, hi] = ContextEntry {
            domain: id,
            page_table: top.phys,
            levels: unit.levels,
            fault_processing_disable: false,
        }
        .encode()
        .ok_or("a context entry does not encode")?;
        let devfn = u64::from(r.device & 0x1F) << 3 | u64::from(r.function & 7);
        // The high half first, so the entry turns present (low bit 0) only when complete.
        context.write64(16 * devfn + 8, hi);
        fence(Ordering::SeqCst);
        context.write64(16 * devfn, lo);
        if !coherent {
            flush(context.virt + 16 * devfn, 16);
        }
        if unit.cap.caching_mode {
            // Caching mode may have cached the entry while it was not present, tagged with
            // domain 0 (VT-d chapter 6).
            let drain = unit.drain();
            unit.invalidate(&[
                Descriptor::ContextDevice {
                    domain: 0,
                    source_id: r.source_id(),
                    function_mask: 0,
                },
                Descriptor::IotlbDomain { domain: id, drain },
            ])?;
        } else {
            unit.write_buffer_flush()?;
        }
        self.domains.push(DomainState {
            unit: ui,
            id,
            requester: r,
            top: top.phys,
            window: IovaWindow::new(vtd::IOVA_BASE, vtd::IOVA_END),
            mapped: 0,
            mapped_ever: 0,
            tables: 1,
        });
        Ok(Domain(self.domains.len() - 1))
    }

    fn map(&mut self, d: Domain, phys: u64) -> Result<u64, &'static str> {
        let dom = self.domains.get_mut(d.0).ok_or("no such domain")?;
        let unit = self.units.get_mut(dom.unit).ok_or("no unit")?;
        let iova = dom
            .window
            .alloc(1)
            .ok_or("the domain's IOVA window is full")?;
        let mut tables = Tables {
            coherent: unit.ecap.coherent,
            made: 0,
        };
        // SNP where the unit offers snoop control: a device's No Snoop cannot then bypass
        // the caches every DMA buffer lives in (VT-d §3.10).
        vtd::map(
            &mut tables,
            dom.top,
            unit.levels,
            iova,
            phys,
            Access::READ_WRITE,
            unit.ecap.snoop_control,
        )
        .map_err(|_| "the page could not be mapped")?;
        dom.tables += tables.made;
        dom.mapped += 1;
        dom.mapped_ever += 1;
        if unit.cap.caching_mode {
            unit.invalidate(&[Descriptor::IotlbPages {
                domain: dom.id,
                address: iova,
                order: 0,
                leaf_only: false,
                drain: Drain::default(),
            }])?;
        } else {
            unit.write_buffer_flush()?;
        }
        Ok(iova)
    }

    fn unmap(&mut self, d: Domain, iova: u64) -> Result<u64, &'static str> {
        let dom = self.domains.get_mut(d.0).ok_or("no such domain")?;
        let unit = self.units.get_mut(dom.unit).ok_or("no unit")?;
        let mut tables = Tables {
            coherent: unit.ecap.coherent,
            made: 0,
        };
        let phys = vtd::unmap(&mut tables, dom.top, unit.levels, iova)
            .map_err(|_| "the page was not mapped")?;
        dom.mapped -= 1;
        // The IOTLB may still hold the translation: invalidate it (and drain requests already
        // translated) before the page is reused.
        let drain = unit.drain();
        let invalidation = if unit.cap.page_invalidation {
            Descriptor::IotlbPages {
                domain: dom.id,
                address: iova,
                order: 0,
                leaf_only: true,
                drain,
            }
        } else {
            Descriptor::IotlbDomain {
                domain: dom.id,
                drain,
            }
        };
        unit.invalidate(&[invalidation])?;
        Ok(phys)
    }

    /// Writes IRTE `index` of unit `ui` (`None`: clears it) and invalidates its cache entry.
    fn write_irte(
        &mut self,
        ui: usize,
        index: u16,
        entry: Option<[u64; 2]>,
    ) -> Result<(), &'static str> {
        let unit = self.units.get_mut(ui).ok_or("no unit")?;
        let irt = unit
            .irt
            .as_ref()
            .ok_or("its unit does not remap interrupts")?;
        let at = 16 * u64::from(index);
        match entry {
            // The high half first, so the entry turns present only when complete.
            Some([lo, hi]) => {
                irt.table.write64(at + 8, hi);
                fence(Ordering::SeqCst);
                irt.table.write64(at, lo);
            }
            // Not present first.
            None => {
                irt.table.write64(at, 0);
                fence(Ordering::SeqCst);
                irt.table.write64(at + 8, 0);
            }
        }
        if !unit.ecap.coherent {
            flush(irt.table.virt + at, 16);
        }
        unit.invalidate(&[Descriptor::IecIndex { index, order: 0 }])
    }

    /// Allocates an IRTE on `r`'s unit; returns the unit and the index.
    fn alloc_irte(&mut self, r: Requester) -> Result<(usize, u16), &'static str> {
        let ui = self.unit_of(r)?;
        let irt = self
            .units
            .get_mut(ui)
            .and_then(|u| u.irt.as_mut())
            .ok_or("its unit does not remap interrupts")?;
        let index = irt
            .used
            .alloc()
            .ok_or("the interrupt remapping table is full")?;
        Ok((ui, index as u16))
    }

    fn route(&mut self, r: Requester, apic: u32, vector: u8) -> Result<MsiMessage, &'static str> {
        let ui = self.unit_of(r)?;
        let x2apic = self
            .units
            .get(ui)
            .and_then(|u| u.irt.as_ref())
            .ok_or("its unit does not remap interrupts")?
            .x2apic;
        let entry = Irte {
            vector,
            destination: apic,
            source: SourceCheck::RequesterId {
                source_id: r.source_id(),
                qualifier: 0,
            },
            level: false,
            fault_processing_disable: false,
        }
        .encode(x2apic)
        .ok_or("the destination needs extended interrupt mode, or the vector is reserved")?;
        let (ui, index) = self.alloc_irte(r)?;
        self.write_irte(ui, index, Some(entry))?;
        Ok(vtd::remappable_msi(index))
    }

    /// Routes a pin of the I/O APIC whose ID is `ioapic` through a new IRTE on the unit whose
    /// DMAR device scope lists that I/O APIC: `vector` to the processor with x2APIC ID
    /// `apic`, with the pin's trigger mode (`level`), checking the source ID the scope names.
    fn route_pin(
        &mut self,
        ioapic: u8,
        apic: u32,
        vector: u8,
        level: bool,
    ) -> Result<PinRoute, &'static str> {
        let (listed, requester) = self.dmar.ioapic(ioapic, &mut Config).map_err(|e| match e {
            IoApicScopeError::NotListed => "no remapping unit's DMAR scope lists this I/O APIC",
            IoApicScopeError::ListedTwice => "the DMAR lists this I/O APIC twice",
            IoApicScopeError::Unresolved => "this I/O APIC's DMAR scope does not resolve",
        })?;
        let ui = self
            .units
            .iter()
            .position(|u| u.base == listed.register_base)
            .ok_or("its remapping unit is not running")?;
        let irt = self
            .units
            .get_mut(ui)
            .and_then(|u| u.irt.as_mut())
            .ok_or("its unit does not remap interrupts")?;
        let source_id = requester.source_id();
        let entry = Irte {
            vector,
            destination: apic,
            source: SourceCheck::RequesterId {
                source_id,
                qualifier: 0,
            },
            level,
            fault_processing_disable: false,
        };
        let encoded = entry
            .encode(irt.x2apic)
            .ok_or("the destination needs extended interrupt mode, or the vector is reserved")?;
        let index = irt
            .used
            .alloc()
            .ok_or("the interrupt remapping table is full")? as u16;
        if let Err(why) = self.write_irte(ui, index, Some(encoded)) {
            // The entry may be present in memory: free its slot only once it is cleared.
            if self.write_irte(ui, index, None).is_ok()
                && let Some(irt) = self.units.get_mut(ui).and_then(|u| u.irt.as_mut())
            {
                irt.used.clear(usize::from(index));
            }
            return Err(why);
        }
        Ok(PinRoute {
            unit: ui,
            index,
            source_id,
            entry,
        })
    }

    fn irte(&self, r: Requester, index: u16) -> Option<Irte> {
        let irt = self.units.get(self.unit_of(r).ok()?)?.irt.as_ref()?;
        let at = 16 * u64::from(index);
        Irte::decode([irt.table.read64(at), irt.table.read64(at + 8)], irt.x2apic)
    }

    /// The IRTE a remappable-format message with data 0 selects, read back.
    fn lookup(&self, r: Requester, m: MsiMessage) -> Option<Route> {
        if m.data != 0 {
            return None;
        }
        let index = vtd::interrupt_index(m)?;
        let e = self.irte(r, index)?;
        Some(Route {
            index,
            vector: e.vector,
            destination: e.destination,
            source_checked: e.source
                == SourceCheck::RequesterId {
                    source_id: r.source_id(),
                    qualifier: 0,
                },
        })
    }
}

/// Attaches the PCI function at `at` to a domain of its own (created on first use): from
/// then on its DMA is translated by that domain's tables, which map nothing until [`map`].
pub fn attach(at: Address) -> Result<Domain, &'static str> {
    if amd::active() {
        return amd::attach(at);
    }
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        let s = g.as_mut().ok_or("no IOMMU")?;
        if !s.translating {
            return Err("DMA remapping is off");
        }
        s.attach(requester(at))
    })
}

/// Maps the physical 4 KiB page `phys` into domain `d`, readable and writable by the
/// device; returns the IOVA the device must use (never `phys` itself).
pub fn map(d: Domain, frame: DmaFrame) -> Result<Iova, &'static str> {
    let phys = frame.phys().raw();
    let iova = if amd::active() {
        amd::map(d, phys)?
    } else {
        crate::cpu::without_interrupts(|| IOMMU.lock().as_mut().ok_or("no IOMMU")?.map(d, phys))?
    };
    Ok(Iova::new(iova))
}

/// Unmaps `iova` from domain `d` and returns the page it mapped. When this returns, the
/// IOMMU's translation cache no longer holds the translation, so the device cannot reach the
/// page.
pub fn unmap(d: Domain, iova: Iova) -> Result<u64, &'static str> {
    if amd::active() {
        return amd::unmap(d, iova.raw());
    }
    crate::cpu::without_interrupts(|| {
        IOMMU
            .lock()
            .as_mut()
            .ok_or("no IOMMU")?
            .unmap(d, iova.raw())
    })
}

/// How domain `d`'s tables translate `iova`, walked in software as the hardware would.
pub fn translate(d: Domain, iova: Iova) -> Option<(u64, Access)> {
    let iova = iova.raw();
    if amd::active() {
        return amd::translate(d, iova);
    }
    crate::cpu::without_interrupts(|| {
        let g = IOMMU.lock();
        let s = g.as_ref()?;
        let dom = s.domains.get(d.0)?;
        let unit = s.units.get(dom.unit)?;
        let mut tables = Tables {
            coherent: unit.ecap.coherent,
            made: 0,
        };
        vtd::translate(&mut tables, dom.top, unit.levels, iova)
    })
}

/// The message the function at `at` must send to interrupt the processor with x2APIC ID
/// `apic` at `vector`. With interrupt remapping on: a handle to a new IRTE that holds the
/// vector and destination — under VT-d a remappable-format handle whose IRTE checks the
/// device's source ID, under AMD-Vi an index into the device's own table — and any 32-bit
/// APIC ID where the IOMMU has x2APIC destinations. Without it: compatibility format, which
/// reaches APIC IDs 0–255 only.
pub fn msi_message(at: Address, apic: u32, vector: u8) -> Result<MsiMessage, &'static str> {
    if amd::active() {
        return amd::msi_message(at, apic, vector);
    }
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        match g.as_mut() {
            Some(s) if s.remapping => s.route(requester(at), apic, vector),
            _ => Ok(MsiMessage {
                address: pci::msi_address(apic)
                    .ok_or("APIC IDs above 255 need interrupt remapping")?,
                data: pci::msi_data(vector).ok_or("a reserved vector")?,
            }),
        }
    })
}

/// Whether interrupts are remapped.
pub fn remapping() -> bool {
    if amd::active() {
        return amd::translating();
    }
    crate::cpu::without_interrupts(|| IOMMU.lock().as_ref().is_some_and(|s| s.remapping))
}

/// Where a message from `at` goes, read back from its IRTE: `None` unless `m` is a handle to
/// a present IRTE in the form `msi_message` writes.
pub fn route(at: Address, m: MsiMessage) -> Option<Route> {
    if amd::active() {
        return amd::route(at, m);
    }
    crate::cpu::without_interrupts(|| IOMMU.lock().as_ref()?.lookup(requester(at), m))
}

/// Allocates an IRTE on `at`'s unit and leaves it **not present**: a message through it is
/// blocked and recorded as a fault (for the interrupt-remapping proof). Returns the message
/// and the index.
pub fn reserve_blocked_interrupt(at: Address) -> Result<(MsiMessage, u16), &'static str> {
    if amd::active() {
        return amd::reserve_blocked_interrupt(at);
    }
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        let s = g.as_mut().ok_or("no IOMMU")?;
        let (ui, index) = s.alloc_irte(requester(at))?;
        s.write_irte(ui, index, None)?;
        Ok((vtd::remappable_msi(index), index))
    })
}

/// Clears IRTE `index` on `at`'s unit, invalidates its cache entry and frees it.
pub fn release_interrupt(at: Address, index: u16) -> Result<(), &'static str> {
    if amd::active() {
        return amd::release_interrupt(at, index);
    }
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        let s = g.as_mut().ok_or("no IOMMU")?;
        let ui = s.unit_of(requester(at))?;
        s.write_irte(ui, index, None)?;
        if let Some(irt) = s.units.get_mut(ui).and_then(|u| u.irt.as_mut()) {
            irt.used.clear(usize::from(index));
        }
        Ok(())
    })
}

/// An I/O APIC pin routed through an IRTE ([`route_ioapic_pin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinRoute {
    /// The unit whose table holds the entry.
    unit: usize,
    /// The IRTE index: the handle the pin's remappable-format redirection entry carries.
    pub index: u16,
    /// The I/O APIC's source ID, from its DMAR device scope: what the IRTE checks.
    pub source_id: u16,
    /// The entry as written.
    pub entry: Irte,
}

/// Routes a pin of the I/O APIC whose ID (MADT and DMAR enumeration ID) is `ioapic` through
/// a new IRTE: `vector` to the processor with x2APIC ID `apic` — any 32-bit ID with extended
/// interrupt mode — with the pin's trigger mode (`level`; the redirection entry must repeat
/// it), checking the I/O APIC's source ID from the DMAR's device scope, so a message with
/// any other source ID is blocked (ADR-0007 I8; QEMU cannot show this: its I/O APIC sends
/// no source ID, so its unit has none to check). The caller then programs the pin's
/// redirection entry in remappable format with the returned index. Refused under AMD-Vi
/// (not yet: [`routes_ioapic_pins`]), when interrupts are not remapped, or when the DMAR
/// lists no unit for this I/O APIC.
pub fn route_ioapic_pin(
    ioapic: u8,
    apic: u32,
    vector: u8,
    level: bool,
) -> Result<PinRoute, &'static str> {
    if amd::active() {
        return Err("I/O APIC pins are not routed through AMD-Vi interrupt tables yet");
    }
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        let s = g.as_mut().ok_or("no IOMMU")?;
        if !s.remapping {
            return Err("interrupt remapping is off");
        }
        s.route_pin(ioapic, apic, vector, level)
    })
}

/// The IRTE of `pin` as its unit's table holds it now; `None` if it is not present.
pub fn pin_irte(pin: &PinRoute) -> Option<Irte> {
    crate::cpu::without_interrupts(|| {
        let g = IOMMU.lock();
        let irt = g.as_ref()?.units.get(pin.unit)?.irt.as_ref()?;
        let at = 16 * u64::from(pin.index);
        Irte::decode([irt.table.read64(at), irt.table.read64(at + 8)], irt.x2apic)
    })
}

/// For the proofs: rewrites `pin`'s IRTE as `entry` (`None`: not present) and invalidates
/// its cache entry, so the next message through it sees the change. `Some(pin.entry)`
/// restores the route.
pub fn rewrite_pin(pin: &PinRoute, entry: Option<Irte>) -> Result<(), &'static str> {
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        let s = g.as_mut().ok_or("no IOMMU")?;
        let x2apic = s
            .units
            .get(pin.unit)
            .and_then(|u| u.irt.as_ref())
            .ok_or("its unit does not remap interrupts")?
            .x2apic;
        let encoded = match entry {
            Some(e) => Some(e.encode(x2apic).ok_or("the entry does not encode")?),
            None => None,
        };
        s.write_irte(pin.unit, pin.index, encoded)
    })
}

/// Clears `pin`'s IRTE, invalidates its cache entry and frees it. Mask the pin's
/// redirection entry first: a message through a cleared IRTE is blocked and recorded.
pub fn release_pin(pin: &PinRoute) -> Result<(), &'static str> {
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        let s = g.as_mut().ok_or("no IOMMU")?;
        s.write_irte(pin.unit, pin.index, None)?;
        if let Some(irt) = s.units.get_mut(pin.unit).and_then(|u| u.irt.as_mut()) {
            irt.used.clear(usize::from(pin.index));
        }
        Ok(())
    })
}

/// Whether I/O APIC pins are routed through IRTEs ([`route_ioapic_pin`]): under VT-d with
/// interrupt remapping on. Not yet under AMD-Vi, where the I/O APIC's device table entry
/// (DeviceID 00A0h in QEMU's IVRS) blocks its fixed interrupts until it is given an
/// interrupt table of its own (ADR-0007).
pub fn routes_ioapic_pins() -> bool {
    !amd::active() && remapping()
}

/// Reads and clears every unit's recorded faults; returns them and whether any were lost (a
/// VT-d recording register was still full, or an AMD-Vi event log overflowed).
pub fn take_faults() -> (Vec<Fault>, bool) {
    if amd::active() {
        return amd::take_faults();
    }
    crate::cpu::without_interrupts(|| {
        let mut g = IOMMU.lock();
        let Some(s) = g.as_mut() else {
            return (Vec::new(), false);
        };
        let mut all = Vec::new();
        let mut lost = false;
        for u in &mut s.units {
            let (f, o) = u.take_faults();
            all.extend(f.into_iter().map(Fault::from_vtd));
            lost |= o;
        }
        (all, lost)
    })
}

/// One attached device, for the report.
#[derive(Clone, Copy, Debug)]
pub struct DomainInfo {
    pub at: Address,
    pub id: u16,
    pub mapped: u64,
    pub mapped_ever: u64,
    pub tables: u64,
}

/// Every attached device's domain, and the IRTEs in use on all units.
pub fn domains() -> (Vec<DomainInfo>, usize) {
    if amd::active() {
        return amd::domains();
    }
    crate::cpu::without_interrupts(|| {
        let g = IOMMU.lock();
        let Some(s) = g.as_ref() else {
            return (Vec::new(), 0);
        };
        let list = s
            .domains
            .iter()
            .map(|d| DomainInfo {
                at: Address {
                    bus: d.requester.bus,
                    device: d.requester.device,
                    function: d.requester.function,
                },
                id: d.id,
                mapped: d.mapped,
                mapped_ever: d.mapped_ever,
                tables: d.tables,
            })
            .collect();
        let irtes = s
            .units
            .iter()
            .filter_map(|u| u.irt.as_ref())
            .map(|t| t.used.len() - t.used.free())
            .sum();
        (list, irtes)
    })
}

/// The 16-bit source ID (AMD-Vi: DeviceID) the function at `at`'s requests carry.
pub fn source_id(at: Address) -> u16 {
    if amd::active() {
        return amd::source_id(at);
    }
    requester(at).source_id()
}

/// Where an interrupt from a device goes, read back from its IRTE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    /// The IRTE's index (in the unit's table under VT-d, in the device's own table under
    /// AMD-Vi).
    pub index: u16,
    pub vector: u8,
    /// The destination APIC ID.
    pub destination: u32,
    /// Only the device can use the IRTE: VT-d checks its source ID; AMD-Vi reaches the
    /// table only through the device's own device table entry.
    pub source_checked: bool,
}

/// What a recorded fault was about, whichever IOMMU recorded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// A DMA to a page its domain does not map for that access (VT-d reasons 5 and 6, where
    /// not-present and not-writable read the same; AMD-Vi an I/O page fault with PR clear).
    PageNotMapped,
    /// A DMA to a present page without the access it needed (AMD-Vi PR set).
    PageDenied,
    /// An interrupt through a remapping entry that is not present (VT-d reason 22h; AMD-Vi
    /// an I/O page fault for an interrupt, PR clear: RemapEn clear), or, under AMD-Vi, from
    /// a device whose interrupts its DTE blocks (an invalid device request, Type 101b).
    InterruptBlocked,
    Other,
}

/// The IOMMU's own record of a fault.
#[derive(Clone, Copy, Debug)]
pub enum Detail {
    Vtd(FaultRecord),
    Amd(amdvi::Event),
}

/// One recorded fault.
#[derive(Clone, Copy, Debug)]
pub struct Fault {
    /// The requester ID (DeviceID) the request carried.
    pub source_id: u16,
    /// An interrupt request (else a DMA).
    pub interrupt: bool,
    /// The device address of a DMA (AMD-Vi: also of an interrupt).
    pub address: u64,
    /// The IRTE index of an interrupt fault, where the IOMMU records it (VT-d).
    pub interrupt_index: Option<u16>,
    /// A write, where the IOMMU says.
    pub write: Option<bool>,
    pub kind: FaultKind,
    pub detail: Detail,
}

impl Fault {
    fn from_vtd(r: FaultRecord) -> Self {
        Self {
            source_id: r.source_id,
            interrupt: r.is_interrupt(),
            address: r.address,
            interrupt_index: r.interrupt_index,
            write: (!r.is_interrupt()).then_some(!r.read),
            kind: match r.reason {
                5 | 6 => FaultKind::PageNotMapped,
                0x22 => FaultKind::InterruptBlocked,
                _ => FaultKind::Other,
            },
            detail: Detail::Vtd(r),
        }
    }
}

/// Why an AMD-Vi I/O page fault happened, from its flags (§2.5.3) and whether it was an
/// interrupt (`Fault::interrupt`).
fn amd_cause(e: &amdvi::Event, interrupt: bool) -> &'static str {
    match (interrupt, e.present()) {
        (false, false) => "page not present",
        (true, false) => "interrupt remapping entry not enabled",
        _ if e.reserved_bit() => "reserved bit set",
        _ if e.permission() => "access not permitted",
        _ => "bad entry",
    }
}

impl core::fmt::Display for Fault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.detail {
            Detail::Vtd(r) => {
                write!(f, "source ID {:#06x}, ", r.source_id)?;
                match (r.is_interrupt(), r.interrupt_index) {
                    (_, Some(i)) => write!(f, "interrupt index {i}")?,
                    (true, None) => f.write_str("interrupt")?,
                    (false, None) => write!(
                        f,
                        "{} at {:#x}",
                        if r.read { "read" } else { "write" },
                        r.address
                    )?,
                }
                write!(
                    f,
                    ", reason {:#x} ({})",
                    r.reason,
                    vtd::fault_reason(r.reason)
                )
            }
            Detail::Amd(e) => {
                write!(f, "DeviceID {:#06x}, ", e.device)?;
                let access = match (self.interrupt, self.write) {
                    (true, _) => "interrupt",
                    (false, Some(true)) => "write",
                    (false, Some(false)) => "read",
                    (false, None) => "access",
                };
                write!(f, "{access} at {:#x}", e.address)?;
                // Only some entries record a DomainID (Tables 57, 60).
                if let Some(domain) = e.domain() {
                    write!(f, ", domain {domain}")?;
                }
                write!(f, ", event {:#x} ({}", e.code, amdvi::event_name(e.code))?;
                if e.code == amdvi::event::IO_PAGE_FAULT {
                    write!(f, ": {}", amd_cause(&e, self.interrupt))?;
                }
                if let Some(kind) = e.kind() {
                    write!(f, ", type {kind:#05b}")?;
                }
                f.write_str(")")
            }
        }
    }
}

/// A fault the proofs looked for, or "none".
#[derive(Clone, Copy, Debug)]
pub struct Recorded(pub Option<Fault>);

impl core::fmt::Display for Recorded {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(r) => write!(f, "{r}"),
            None => f.write_str("none"),
        }
    }
}

/// How MSI-X entries name their IRTE under the running IOMMU, for the proofs' report.
pub fn routing_words() -> &'static str {
    if amd::active() {
        "hold only an index into their device's own interrupt remapping table, whose IRTE holds the vector and the x2APIC destination"
    } else {
        "hold only a remappable handle with data 0 whose IRTE holds the vector, the x2APIC destination and the device's source ID"
    }
}

/// What an unmap waited for, for the proofs' report.
pub fn unmap_words() -> &'static str {
    if amd::active() {
        "the translation cache invalidated page-selectively and the completion awaited"
    } else {
        "IOTLB invalidated page-selectively, reads and writes drained"
    }
}

/// Whether the interrupt proof must find the blocked interrupt in the IOMMU's fault record.
/// VT-d records it (reason 22h), and QEMU's model does. AMD-Vi logs it too (an I/O page fault,
/// §2.5.3, Table 44), but QEMU 11.1's `amd-iommu` logs no interrupt-remapping fault (it only prints
/// one on its stderr), so under AMD-Vi the proof requires only that the interrupt was not
/// delivered, and the boot test expects the record to be absent: a QEMU that starts logging is
/// noticed. Hardware qualification must require the record (§74).
pub fn interrupt_record_required() -> bool {
    !amd::active()
}

/// Whether the DMA proof must find the blocked IOVA in the fault record's address. VT-d
/// records it, and QEMU's model does. AMD-Vi records it too (§2.5.3), but QEMU 11.1's
/// `amd-iommu` writes every event with address 0: its event encoder stores the address
/// through a 64-bit-wide mask built with a shift by 64, undefined in C and empty in practice.
/// So under AMD-Vi the proof also accepts address 0 — never another address — and the boot
/// test expects 0: a QEMU that starts recording the address is noticed. Hardware
/// qualification must require the address (§74).
pub fn fault_address_required() -> bool {
    !amd::active()
}

/// What confines an unattached device, for the proofs' report.
pub fn default_deny_words() -> &'static str {
    if amd::active() {
        "every other function's device table entry blocks its DMA and fixed interrupts"
    } else {
        "no other function has a context entry, so its DMA faults"
    }
}
