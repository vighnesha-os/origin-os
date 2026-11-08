//! AMD IOMMU (AMD-Vi) DMA and interrupt remapping (CLAUDE.md §32 AMD IOMMU and interrupt
//! remapping, §58 DMA protection; ADR-0007, Proposed; research ledger
//! `docs/research/2026-11-08-amd-vi-iommu.md`), behind the parent module's entry points, so
//! drivers do not change: they attach their device, take DMA pages that come mapped, and
//! program MSI-X entries through `iommu::msi_message`.
//!
//! At boot, when the firmware describes AMD IOMMUs (an ACPI IVRS, no DMAR), after PCIe
//! enumeration and before any driver binds:
//! 1. the IVRS names the IOMMUs and the DeviceIDs each translates (`framekernel::ivrs`,
//!    checked whole); each IOMMU is taken from its richest IVHD;
//! 2. each IOMMU's capability block is read from its PCI function and its registers are
//!    mapped uncached; the firmware must have left it disabled;
//! 3. a device table reaching the highest DeviceID the IVHD names is built with **every
//!    entry blocking** (ADR-0007 I2): valid with translation information not valid, so
//!    every DMA is target aborted and logged and none passes untranslated; interrupt
//!    remapping valid with fixed and arbitrated interrupts target aborted; only the special
//!    interrupts the firmware asks to pass (the IVRS DTE settings) get through;
//! 4. a command buffer and an event log (one page, 256 entries each) are set and the IOMMU
//!    is enabled — with 128-bit IRTEs and 32-bit x2APIC destinations (GAEn and XTEn) where
//!    it has both GASup and XTSup — then every cache and every device table entry is
//!    invalidated and the completion awaited.
//!
//! A driver then attaches its device ([`attach`]): it gets a domain (an ID from 1, 4-level
//! host page tables, the IOVA window [1 GiB, 2 GiB)) and an interrupt remapping table of its
//! own, and its device table entry turns to translation. Each DMA page is mapped at an IOVA,
//! never at its physical address ([`map`]); unmapping invalidates the page in the IOMMU's
//! translation cache and waits for the completion ([`unmap`]). An MSI-X entry carries only an
//! index into the device's own table; the vector and destination live in the IRTE. Because
//! the table belongs to the device's DTE, no other device can use it: the source check is
//! structural. Faults are read from the event log ([`take_faults`]).
//!
//! Every structure is a zeroed kernel page (or run of pages) kept for ever. The IOMMU snoops
//! page-table walks (DTE SD clear), interrupt-table walks and command reads by definition,
//! and device-table reads with Control.Coherent, set as the IVHD recommends; were it not,
//! device table lines are flushed after each write. The `unsafe` here is volatile access to
//! those structures and their allocation; MMIO goes through the parent's `Regs`.

use alloc::alloc::{Layout, alloc_zeroed};
use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering, fence};

use framekernel::amdvi::{
    self, Capability, Command, Dma, Dte, Event, Features, Interrupts, Irte, MsiMessage, Pages,
    control, reg, status,
};
use framekernel::bitmap::Bitmap;
use framekernel::ivrs::{self, Ivrs, Special};
use framekernel::pci::Address;
use framekernel::vtd::{Access, IovaWindow};
use origin_sync::SpinLock;

use super::{Domain, DomainInfo, Fault, FaultKind, PAGE, Page, Regs, Route, Tables, poll};
use crate::console::Console;

/// Each ring (command buffer, event log) is one page: 2^8 entries of 16 bytes.
const RING_ORDER: u8 = 8;
const RING_ENTRIES: usize = 1 << RING_ORDER;
/// Each device's interrupt remapping table: 2^8 entries, every one reachable with a fixed
/// message type ([`amdvi::FIXED_INDEXES`]); 16 bytes each at most, so one page.
const IRT_ORDER: u8 = 8;
const IRT_ENTRIES: usize = 1 << IRT_ORDER;
/// Host page-table depth for every domain: 4 levels, 48-bit IOVAs. Every IOMMU supports it
/// (HATS reports at least 4).
const LEVELS: u8 = 4;
/// Commands queued before a completion wait (the ring holds 256).
const BATCH: usize = 120;
/// IOMMUs the kernel drives.
const MAX_UNITS: usize = 8;

/// Zeroed, page-aligned kernel memory the IOMMU reads, kept for ever (the device table).
#[derive(Clone, Copy, Debug)]
struct Region {
    virt: u64,
    phys: u64,
    len: u64,
}

impl Region {
    fn new(len: u64) -> Option<Self> {
        let layout = Layout::from_size_align(usize::try_from(len).ok()?, PAGE as usize).ok()?;
        if len == 0 {
            return None;
        }
        // SAFETY: `layout` has a non-zero size (checked above). The memory is never freed:
        // the IOMMU may read it at any time. A run of frames from the heap is physically
        // contiguous and reached through the direct map, so `to_phys` of its start is the
        // physical start of the whole run.
        let p = unsafe { alloc_zeroed(layout) };
        if p.is_null() {
            return None;
        }
        let virt = p.expose_provenance() as u64;
        Some(Self {
            virt,
            phys: crate::phys::to_phys(virt),
            len,
        })
    }

    fn read64(&self, offset: u64) -> u64 {
        if offset.checked_add(8).is_none_or(|end| end > self.len) || !offset.is_multiple_of(8) {
            return 0;
        }
        let p = ptr::with_exposed_provenance::<u64>((self.virt + offset) as usize);
        // SAFETY: inside this live, never-freed region (checked above), 8-byte aligned. The
        // IOMMU may read it concurrently, so the access is volatile and no reference is made.
        unsafe { p.read_volatile() }
    }

    fn write64(&self, offset: u64, value: u64) {
        if offset.checked_add(8).is_none_or(|end| end > self.len) || !offset.is_multiple_of(8) {
            return;
        }
        let p = ptr::with_exposed_provenance_mut::<u64>((self.virt + offset) as usize);
        // SAFETY: as for `read64`. The region holds device table entries only this module
        // writes.
        unsafe { p.write_volatile(value) };
    }
}

/// One running IOMMU.
struct Unit {
    /// Physical base of its registers (its identity in the IVRS).
    base: u64,
    regs: Regs,
    capability: Capability,
    /// The features used: the register and the IVRS image both report them.
    features: Features,
    efr: u64,
    efr_register: u64,
    efr_image: Option<u64>,
    efr2: u64,
    /// Device-table reads are snooped (Control.Coherent); else entries are flushed.
    coherent: bool,
    table: Region,
    entries: u32,
    commands: Page,
    tail: usize,
    events: Page,
    /// Where COMPLETION_WAIT writes its store data, and the last value used.
    status: Page,
    sequence: u64,
    /// 128-bit IRTEs with 32-bit x2APIC destinations (GAEn and XTEn set).
    wide: bool,
    control: u64,
    next_domain: u32,
}

/// One device's domain.
#[derive(Clone, Copy, Debug)]
struct DomainState {
    unit: usize,
    id: u16,
    /// The DeviceID its requests carry (the device table entry used).
    requester: u16,
    at: Address,
    /// Physical address of its top-level page table.
    top: u64,
    window: IovaWindow,
    mapped: u64,
    mapped_ever: u64,
    /// Table pages (the top one included).
    tables: u64,
}

/// One device's interrupt remapping table.
struct InterruptTable {
    unit: usize,
    requester: u16,
    table: Page,
    used: Bitmap,
}

struct AmdVi {
    ivrs: Ivrs<'static>,
    units: Vec<Unit>,
    domains: Vec<DomainState>,
    interrupts: Vec<InterruptTable>,
}

static AMD: SpinLock<Option<AmdVi>> = SpinLock::new(None);
/// Set once the IOMMUs run; the parent's entry points then come here.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether AMD-Vi drives DMA and interrupt remapping.
pub(super) fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

fn address(id: u16) -> Address {
    let (bus, device, function) = ivrs::bdf(id);
    Address {
        bus,
        device,
        function,
    }
}

fn device_id(at: Address) -> u16 {
    ivrs::device_id(at.bus, at.device, at.function)
}

impl Unit {
    /// The four words of DeviceID `device`'s entry.
    fn dte(&self, device: u16) -> [u64; 4] {
        let at = u64::from(device) * amdvi::DTE_BYTES;
        [0, 8, 16, 24].map(|w| self.table.read64(at + w))
    }

    /// Writes DeviceID `device`'s entry: the words that change, the interrupt word before the
    /// translation words and word 0 (valid, mode, root, permissions) last, so a translation
    /// turns on only once its domain is in place. The caller invalidates the entry.
    fn set_dte(&mut self, device: u16, dte: Dte) -> Result<(), &'static str> {
        if u32::from(device) >= self.entries {
            return Err("the DeviceID lies beyond the device table");
        }
        let new = dte.encode().ok_or("a device table entry does not encode")?;
        let old = self.dte(device);
        let at = u64::from(device) * amdvi::DTE_BYTES;
        for w in [3usize, 2, 1, 0] {
            if old[w] != new[w] {
                self.table.write64(at + 8 * w as u64, new[w]);
                fence(Ordering::SeqCst);
            }
        }
        if !self.coherent {
            super::flush(self.table.virt + at, amdvi::DTE_BYTES);
        }
        Ok(())
    }

    /// Queues `commands` and a COMPLETION_WAIT that stores a new sequence number, then waits
    /// (bounded) until the IOMMU has stored it: every command before it has completed.
    fn submit(&mut self, commands: &[Command]) -> Result<(), &'static str> {
        for chunk in commands.chunks(BATCH) {
            self.submit_batch(chunk)?;
        }
        if commands.is_empty() {
            self.submit_batch(&[])?;
        }
        Ok(())
    }

    fn submit_batch(&mut self, commands: &[Command]) -> Result<(), &'static str> {
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let sequence = self.sequence;
        self.status.write64(0, 0);
        let wait = Command::CompletionWait {
            store: self.status.phys,
            data: sequence,
            flush: true,
        };
        for c in commands.iter().chain(core::iter::once(&wait)) {
            let [lo, hi] = c.encode().ok_or("a command does not encode")?;
            let at = amdvi::ring_offset(self.tail);
            self.commands.write64(at, lo);
            self.commands.write64(at + 8, hi);
            self.tail = (self.tail + 1) % RING_ENTRIES;
        }
        fence(Ordering::SeqCst);
        self.regs
            .write64(reg::COMMAND_TAIL, amdvi::ring_offset(self.tail));
        let mut stopped = false;
        let done = poll(|| {
            stopped = self.regs.read64(reg::STATUS) & status::CMD_BUF_RUN == 0;
            stopped || self.status.read64(0) == sequence
        });
        if self.status.read64(0) == sequence {
            Ok(())
        } else if stopped {
            Err("the IOMMU stopped fetching commands (an illegal command)")
        } else if !done {
            Err("a command did not complete")
        } else {
            Err("a completion wait stored the wrong value")
        }
    }

    /// Reads and clears every event in the log, and restarts logging after an overflow
    /// (§2.5.1); returns the events and whether any were lost.
    fn take_events(&mut self) -> (Vec<Event>, bool) {
        let mut out = Vec::new();
        let tail = amdvi::ring_index(self.regs.read64(reg::EVENT_TAIL)) % RING_ENTRIES;
        let mut head = amdvi::ring_index(self.regs.read64(reg::EVENT_HEAD)) % RING_ENTRIES;
        for _ in 0..RING_ENTRIES {
            if head == tail {
                break;
            }
            let at = amdvi::ring_offset(head);
            if let Some(e) = Event::decode(self.events.read64(at), self.events.read64(at + 8)) {
                out.push(e);
            }
            self.events.write64(at, 0);
            self.events.write64(at + 8, 0);
            head = (head + 1) % RING_ENTRIES;
        }
        self.regs.write64(reg::EVENT_HEAD, amdvi::ring_offset(head));
        let st = self.regs.read64(reg::STATUS);
        let overflow = st & status::EVENT_OVERFLOW != 0;
        if overflow {
            self.regs
                .write64(reg::CONTROL, self.control & !control::EVENT_LOG_EN);
            let _ = poll(|| self.regs.read64(reg::STATUS) & status::EVENT_LOG_RUN == 0);
            self.regs.write64(reg::STATUS, status::EVENT_OVERFLOW);
            self.regs.write64(reg::CONTROL, self.control);
        }
        if st & status::EVENT_LOG_INT != 0 {
            self.regs.write64(reg::STATUS, status::EVENT_LOG_INT);
        }
        (out, overflow)
    }

    /// Turns the IOMMU off again (a later IOMMU failed to come up).
    fn disable(&self) {
        self.regs.write64(reg::CONTROL, 0);
    }
}

/// What [`bring_up`] found and did, for the report.
struct Brought {
    unit: Unit,
    stale: usize,
    status: u64,
}

/// Brings up the IOMMU `u`: checks its capability block and registers, builds the blocking
/// device table, sets the command buffer and event log, enables it and invalidates its
/// caches.
fn bring_up(u: &ivrs::Unit<'static>) -> Result<Brought, &'static str> {
    if u.segment != 0 {
        return Err("PCI segments other than 0 are not supported");
    }
    let at = address(u.device_id);
    let header = crate::pcie::read_config32(at, u.capability_offset)
        .ok_or("its capability block cannot be read")?;
    let capability = Capability::decode(header);
    if !capability.is_iommu {
        return Err("its capability offset does not hold an IOMMU capability block");
    }
    let virt = crate::vm::map_mmio(u.base, reg::SPAN).map_err(|_| "cannot map its registers")?;
    let regs = Regs {
        virt,
        size: reg::SPAN,
    };
    if regs.read64(reg::CONTROL) & control::IOMMU_EN != 0 {
        return Err("the firmware left it enabled (taking an IOMMU over is not implemented)");
    }
    let efr_register = if capability.efr {
        regs.read64(reg::EFR)
    } else {
        0
    };
    let efr2 = if capability.efr {
        regs.read64(reg::EFR2)
    } else {
        0
    };
    // A feature is used only if the register and the firmware's image both report it.
    let efr = u.efr.map_or(efr_register, |image| efr_register & image);
    let features = Features::decode(efr);
    if features.host_levels < LEVELS {
        return Err("it does not support 4-level host page tables");
    }
    let last = u.last_device_id().ok_or("its IVHD lists no devices")?;
    let entries = (u32::from(last) + 1).next_multiple_of(amdvi::DTES_PER_PAGE);
    let table = Region::new(u64::from(entries) * amdvi::DTE_BYTES)
        .ok_or("no memory for its device table")?;
    let coherent = u.coherent();
    let page = || Page::new(true).ok_or("no memory for its structures");
    let mut unit = Unit {
        base: u.base,
        regs,
        capability,
        features,
        efr,
        efr_register,
        efr_image: u.efr,
        efr2,
        coherent,
        table,
        entries,
        commands: page()?,
        tail: 0,
        events: page()?,
        status: page()?,
        sequence: 0,
        wide: features.wide_interrupts(),
        control: 0,
        next_domain: 1,
    };
    // Every entry blocks DMA and fixed interrupts; the firmware's pass bits go to the entry
    // the device's requests use (its alias, where it has one).
    let mut settings: Vec<u8> = alloc::vec![0; entries as usize];
    for id in 0..entries {
        if let Some(c) = u.covers(id as u16)
            && let Some(s) = settings.get_mut(usize::from(c.requester))
        {
            *s |= c.dte.0;
        }
    }
    for (id, &setting) in settings.iter().enumerate() {
        let words = Dte {
            dma: Dma::Blocked,
            domain: 0,
            interrupts: Interrupts::Blocked,
            setting,
        }
        .encode()
        .ok_or("a device table entry does not encode")?;
        let at = id as u64 * amdvi::DTE_BYTES;
        for (w, v) in words.iter().enumerate() {
            unit.table.write64(at + 8 * w as u64, *v);
        }
    }
    if !coherent {
        super::flush(unit.table.virt, unit.table.len);
    }
    fence(Ordering::SeqCst);
    let r = &unit.regs;
    // Stale status first: some implementations ignore an event-log base write while
    // EventLogInt is set (QEMU does).
    r.write64(
        reg::STATUS,
        status::EVENT_OVERFLOW | status::EVENT_LOG_INT | status::COM_WAIT_INT,
    );
    r.write64(
        reg::DEVICE_TABLE,
        amdvi::device_table_base(unit.table.phys, entries)
            .ok_or("the device table is misplaced")?,
    );
    r.write64(
        reg::COMMAND_BUFFER,
        amdvi::ring_base(unit.commands.phys, RING_ORDER)
            .ok_or("the command buffer is misplaced")?,
    );
    r.write64(
        reg::EVENT_LOG,
        amdvi::ring_base(unit.events.phys, RING_ORDER).ok_or("the event log is misplaced")?,
    );
    for p in [
        reg::COMMAND_HEAD,
        reg::COMMAND_TAIL,
        reg::EVENT_HEAD,
        reg::EVENT_TAIL,
    ] {
        r.write64(p, 0);
    }
    // No exclusion range: every access is translated and checked.
    r.write64(reg::EXCLUSION_BASE, 0);
    r.write64(reg::EXCLUSION_LIMIT, 0);
    // The control settings the IVHD recommends, then the enables in one write (§2.3.1).
    let flags = u64::from(u.flags);
    let recommended = [
        (ivrs::ivhd_flags::HT_TUNNEL, control::HT_TUN_EN),
        (ivrs::ivhd_flags::PASS_PW, control::PASS_PW),
        (ivrs::ivhd_flags::RES_PASS_PW, control::RES_PASS_PW),
        (ivrs::ivhd_flags::ISOC, control::ISOC),
        (ivrs::ivhd_flags::COHERENT, control::COHERENT),
    ]
    .iter()
    .filter(|(f, _)| flags & u64::from(*f) != 0)
    .fold(0, |acc, (_, c)| acc | c);
    let interrupts = if unit.wide {
        control::GA_EN | control::XT_EN
    } else {
        0
    };
    unit.control =
        recommended | interrupts | control::EVENT_LOG_EN | control::CMD_BUF_EN | control::IOMMU_EN;
    unit.regs.write64(reg::CONTROL, unit.control);
    let running = status::CMD_BUF_RUN | status::EVENT_LOG_RUN;
    if !poll(|| unit.regs.read64(reg::STATUS) & running == running) {
        unit.disable();
        return Err("its command buffer or event log did not start");
    }
    // Nothing the IOMMU cached before belongs to this table: invalidate it all — with
    // INVALIDATE_IOMMU_ALL where it exists, and every device table entry and interrupt table
    // one by one in any case. The second is what the specification asks without IASup; it
    // is also what makes QEMU 11.1 put each device under its entry (it switches a device to
    // translation only on that device's INVALIDATE_DEVTAB_ENTRY), so default deny holds for
    // devices no driver ever attaches.
    let all = features.invalidate_all.then_some(Command::InvalidateAll);
    let flush: Vec<Command> = all
        .into_iter()
        .chain((0..entries).flat_map(|d| {
            [
                Command::InvalidateDevice { device: d as u16 },
                Command::InvalidateInterrupts { device: d as u16 },
            ]
        }))
        .chain(core::iter::once(Command::InvalidatePages {
            domain: 0,
            address: 0,
            pages: Pages::All,
        }))
        .collect();
    if let Err(e) = unit.submit(&flush) {
        unit.disable();
        return Err(e);
    }
    let (stale, _) = unit.take_events();
    let st = unit.regs.read64(reg::STATUS);
    Ok(Brought {
        unit,
        stale: stale.len(),
        status: st,
    })
}

/// Brings up every IOMMU the IVRS (`table`, checked by `firmware::acpi_table`) names.
/// Returns `false` only for a failure.
pub(super) fn init(com1: &mut Console, table: &'static [u8]) -> bool {
    let ivrs = match ivrs::parse(table) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: IOMMU: the IVRS is invalid ({e:?}): IOMMU failed"
            );
            return false;
        }
    };
    let units: Vec<_> = ivrs.units().collect();
    let _ = writeln!(
        com1,
        "origin-kernel: IOMMU: IVRS revision {}, IVinfo {:#x} (EFR support {}, {}-bit physical and {}-bit virtual addresses, pre-boot DMA protection {}); {} IOMMU(s) in {} IVHD(s), {} memory definition(s)",
        ivrs.revision,
        ivrs.info,
        ivrs.efr_supported(),
        ivrs.physical_bits(),
        ivrs.virtual_bits(),
        ivrs.dma_remap(),
        units.len(),
        ivrs.hardware().count(),
        ivrs.memory().count()
    );
    if units.is_empty() || units.len() > MAX_UNITS {
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: {} IOMMUs (1-{MAX_UNITS} supported): IOMMU failed",
            units.len()
        );
        return false;
    }
    for m in ivrs.memory() {
        // Identity maps and exclusion ranges are not implemented yet (ADR-0007): a device an
        // IVMD names is refused at `attach`, so its DMA stays blocked rather than unchecked.
        let _ = writeln!(
            com1,
            "origin-kernel: IOMMU: memory definition {:#x}+{:#x} (type {:#x}, unity {}, read {}, write {}, exclusion {}): not mapped yet, so the devices it names cannot be attached",
            m.start,
            m.length,
            m.kind,
            m.unity(),
            m.read(),
            m.write(),
            m.exclusion()
        );
    }
    let mut running: Vec<Unit> = Vec::with_capacity(units.len());
    for (i, u) in units.iter().enumerate() {
        let entries = u.entries().count();
        let _ = write!(
            com1,
            "origin-kernel: IOMMU: unit {i} at {:#x} (segment {}, IVHD type {:#x} of {}, IOMMU function {}, capability at {:#x}): {entries} device entr{} up to DeviceID {:#06x}",
            u.base,
            u.segment,
            u.kind,
            ivrs.hardware().filter(|v| v.base == u.base).count(),
            address(u.device_id),
            u.capability_offset,
            if entries == 1 { "y" } else { "ies" },
            u.last_device_id().unwrap_or(0)
        );
        for e in u.entries() {
            if let ivrs::DeviceEntry::Special {
                variety: Special::IoApic,
                handle,
                id,
                ..
            } = e
            {
                let _ = write!(com1, ", I/O APIC {handle} (DeviceID {id:#06x})");
            }
        }
        let _ = writeln!(com1);
        match bring_up(u) {
            Ok(b) => {
                let n = &b.unit;
                let f = n.features;
                let _ = writeln!(
                    com1,
                    "origin-kernel: IOMMU: unit {i}: AMD-Vi capability revision {}, IOTLB {}, not-present entries cached {}; EFR {:#x} (register {:#x}, IVRS image {}): x2APIC {}, guest APIC {}, INVALIDATE_IOMMU_ALL {}, {}-level host tables, performance counters {}; EFR2 {:#x}: {} interrupts per function",
                    n.capability.revision,
                    n.capability.iotlb,
                    n.capability.not_present_cached,
                    n.efr,
                    n.efr_register,
                    match n.efr_image {
                        Some(v) => alloc::format!("{v:#x}"),
                        None => alloc::string::String::from("none"),
                    },
                    f.x2apic,
                    f.guest_apic,
                    f.invalidate_all,
                    f.host_levels,
                    f.counters,
                    n.efr2,
                    amdvi::interrupts_per_function(n.efr2)
                );
                let _ = writeln!(
                    com1,
                    "origin-kernel: IOMMU: unit {i}: device table of {} entries at {:#x}, every entry blocking DMA and fixed interrupts; command buffer and event log on (256 entries each); {}; translation on (control {:#x}, status {:#x}){}: every device's DMA is blocked until its driver attaches it; caches invalidated ({}); {} stale event(s) cleared",
                    n.entries,
                    n.table.phys,
                    if n.wide {
                        "interrupt remapping per device (128-bit entries, 32-bit x2APIC destinations)"
                    } else {
                        "interrupt remapping per device (32-bit entries, 8-bit xAPIC destinations)"
                    },
                    n.control,
                    b.status,
                    if n.regs.read64(reg::CONTROL) & control::IOMMU_EN != 0 {
                        ""
                    } else {
                        ": IOMMU failed"
                    },
                    if f.invalidate_all {
                        "INVALIDATE_IOMMU_ALL, then every device table entry and interrupt table"
                    } else {
                        "every device table entry and interrupt table"
                    },
                    b.stale
                );
                running.push(b.unit);
            }
            Err(why) => {
                // The IOMMUs already running would block every driver's DMA with nobody to
                // attach devices to them: turn them off again.
                for u in &running {
                    u.disable();
                }
                let _ = writeln!(com1, "origin-kernel: IOMMU: unit {i}: {why}: IOMMU failed");
                return false;
            }
        }
    }
    crate::cpu::without_interrupts(|| {
        *AMD.lock() = Some(AmdVi {
            ivrs,
            units: running,
            domains: Vec::new(),
            interrupts: Vec::new(),
        });
    });
    ACTIVE.store(true, Ordering::Release);
    true
}

impl AmdVi {
    /// The running IOMMU that translates `at`, and how it sees the device.
    fn resolve(&self, at: Address) -> Result<(usize, ivrs::Coverage), &'static str> {
        let (u, c) = self
            .ivrs
            .unit_for(0, device_id(at))
            .ok_or("no IOMMU translates this device")?;
        let ui = self
            .units
            .iter()
            .position(|x| x.base == u.base)
            .ok_or("its IOMMU is not running")?;
        Ok((ui, c))
    }

    /// The interrupt remapping table of `requester` on unit `ui`, made on first use: the
    /// device table entry then remaps fixed and arbitrated interrupts through it.
    fn interrupt_table(&mut self, ui: usize, requester: u16) -> Result<usize, &'static str> {
        if let Some(i) = self
            .interrupts
            .iter()
            .position(|t| t.unit == ui && t.requester == requester)
        {
            return Ok(i);
        }
        let unit = self.units.get_mut(ui).ok_or("no unit")?;
        let table = Page::new(true).ok_or("no memory for an interrupt remapping table")?;
        let mut dte = Dte::decode(unit.dte(requester))
            .ok_or("its device table entry is not this kernel's")?;
        dte.interrupts = Interrupts::Remapped {
            table: table.phys,
            order: IRT_ORDER,
        };
        unit.set_dte(requester, dte)?;
        unit.submit(&[
            Command::InvalidateInterrupts { device: requester },
            Command::InvalidateDevice { device: requester },
        ])?;
        self.interrupts.push(InterruptTable {
            unit: ui,
            requester,
            table,
            used: Bitmap::new(IRT_ENTRIES),
        });
        Ok(self.interrupts.len() - 1)
    }

    fn attach(&mut self, at: Address) -> Result<Domain, &'static str> {
        let (ui, c) = self.resolve(at)?;
        let requester = c.requester;
        if let Some(i) = self
            .domains
            .iter()
            .position(|d| d.unit == ui && d.requester == requester)
        {
            return Ok(Domain(i));
        }
        let base = self.units.get(ui).ok_or("no unit")?.base;
        let id = device_id(at);
        if self
            .ivrs
            .memory()
            .any(|m| m.unit_base == base && (m.covers(0, id) || m.covers(0, requester)))
        {
            return Err(
                "it needs a memory range identity-mapped or excluded (IVMD), which is not implemented",
            );
        }
        self.interrupt_table(ui, requester)?;
        let unit = self.units.get_mut(ui).ok_or("no unit")?;
        let domain =
            u16::try_from(unit.next_domain).map_err(|_| "its IOMMU has no free domain ID")?;
        // Consumed before any entry names it: an attach that fails below may have left the
        // device table entry naming it, and that domain ID must never go to another device.
        unit.next_domain += 1;
        let top = Page::new(true).ok_or("no memory for a page table")?;
        let mut dte = Dte::decode(unit.dte(requester))
            .ok_or("its device table entry is not this kernel's")?;
        dte.domain = domain;
        dte.dma = Dma::Translated {
            root: top.phys,
            levels: LEVELS,
        };
        unit.set_dte(requester, dte)?;
        unit.submit(&[
            Command::InvalidateDevice { device: requester },
            Command::InvalidatePages {
                domain,
                address: 0,
                pages: Pages::All,
            },
        ])?;
        self.domains.push(DomainState {
            unit: ui,
            id: domain,
            requester,
            at,
            top: top.phys,
            window: IovaWindow::new(amdvi::IOVA_BASE, amdvi::IOVA_END),
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
            coherent: true,
            made: 0,
        };
        amdvi::map(&mut tables, dom.top, LEVELS, iova, phys, Access::READ_WRITE)
            .map_err(|_| "the page could not be mapped")?;
        dom.tables += tables.made;
        dom.mapped += 1;
        dom.mapped_ever += 1;
        if unit.capability.not_present_cached {
            // The IOMMU may have cached the entry while it was not present (NpCache).
            unit.submit(&[Command::InvalidatePages {
                domain: dom.id,
                address: iova,
                pages: Pages::One,
            }])?;
        } else {
            fence(Ordering::SeqCst);
        }
        Ok(iova)
    }

    fn unmap(&mut self, d: Domain, iova: u64) -> Result<u64, &'static str> {
        let dom = self.domains.get_mut(d.0).ok_or("no such domain")?;
        let unit = self.units.get_mut(dom.unit).ok_or("no unit")?;
        let mut tables = Tables {
            coherent: true,
            made: 0,
        };
        let phys = amdvi::unmap(&mut tables, dom.top, LEVELS, iova)
            .map_err(|_| "the page was not mapped")?;
        dom.mapped -= 1;
        // The translation cache may still hold the page: invalidate it, and wait, before the
        // page is reused.
        unit.submit(&[Command::InvalidatePages {
            domain: dom.id,
            address: iova,
            pages: Pages::One,
        }])?;
        Ok(phys)
    }

    /// Writes IRTE `index` of interrupt table `ti` (`None`: RemapEn clear) and invalidates the
    /// device's cached interrupt information.
    fn write_irte(
        &mut self,
        ti: usize,
        index: u16,
        entry: Option<Irte>,
    ) -> Result<(), &'static str> {
        let t = self.interrupts.get(ti).ok_or("no interrupt table")?;
        let unit = self.units.get_mut(t.unit).ok_or("no unit")?;
        if usize::from(index) >= IRT_ENTRIES {
            return Err("no such interrupt table entry");
        }
        if unit.wide {
            let at = 16 * u64::from(index);
            match entry {
                Some(e) => {
                    let [lo, hi] = e.encode_wide(true).ok_or("the vector is reserved")?;
                    // RemapEn is in the low word: written last.
                    t.table.write64(at + 8, hi);
                    fence(Ordering::SeqCst);
                    t.table.write64(at, lo);
                }
                None => {
                    t.table.write64(at, 0);
                    fence(Ordering::SeqCst);
                    t.table.write64(at + 8, 0);
                }
            }
        } else {
            let value = match entry {
                Some(e) => e.encode_basic().ok_or(
                    "the destination needs x2APIC interrupt remapping, or the vector is reserved",
                )?,
                None => 0,
            };
            t.table.write32(4 * u64::from(index), value);
        }
        let requester = t.requester;
        unit.submit(&[Command::InvalidateInterrupts { device: requester }])
    }

    /// A new IRTE index in `at`'s interrupt table, and the table.
    fn alloc_irte(&mut self, at: Address) -> Result<(usize, u16), &'static str> {
        let (ui, c) = self.resolve(at)?;
        let ti = self.interrupt_table(ui, c.requester)?;
        let t = self.interrupts.get_mut(ti).ok_or("no interrupt table")?;
        let index = t
            .used
            .alloc()
            .ok_or("the device's interrupt remapping table is full")?;
        Ok((ti, index as u16))
    }

    fn route(&mut self, at: Address, apic: u32, vector: u8) -> Result<MsiMessage, &'static str> {
        let (ti, index) = self.alloc_irte(at)?;
        let message = amdvi::remapped_msi(index).ok_or("no fixed-type index left")?;
        if let Err(e) = self.write_irte(
            ti,
            index,
            Some(Irte {
                vector,
                destination: apic,
            }),
        ) {
            if let Some(t) = self.interrupts.get_mut(ti) {
                t.used.clear(usize::from(index));
            }
            return Err(e);
        }
        Ok(message)
    }

    /// The IRTE behind `m` from `at`, read back.
    fn lookup(&self, at: Address, m: MsiMessage) -> Option<Route> {
        let index = amdvi::msi_index(m)?;
        let (ui, c) = self.resolve(at).ok()?;
        let t = self
            .interrupts
            .iter()
            .find(|t| t.unit == ui && t.requester == c.requester)?;
        let wide = self.units.get(ui)?.wide;
        let e = if wide {
            let at = 16 * u64::from(index);
            Irte::decode_wide([t.table.read64(at), t.table.read64(at + 8)], true)?
        } else {
            Irte::decode_basic(t.table.read32(4 * u64::from(index)))?
        };
        Some(Route {
            index,
            vector: e.vector,
            destination: e.destination,
            // The table hangs off the device's own device table entry: no other requester
            // can reach it.
            source_checked: true,
        })
    }
}

/// Whether `address` lies in the interrupt address range, where every interrupt request goes
/// (§2.2.5, Table 3).
fn interrupt_address(address: u64) -> bool {
    (0xFEE0_0000..=0xFEEF_FFFF).contains(&address)
}

/// The neutral fault for an event. An I/O page fault counts as an interrupt only if I is set
/// **and** the address is an interrupt address: an interrupt request is a write to that range
/// (§2.2.5), and QEMU 11.1 sets I on every page fault it logs, DMA ones included (research
/// ledger §2.5). On hardware the two agree, so the rule changes nothing there.
fn fault(e: Event) -> Fault {
    let interrupt = e.interrupt() && interrupt_address(e.address);
    let kind = if e.code == amdvi::event::IO_PAGE_FAULT {
        match (interrupt, e.present()) {
            (false, false) => FaultKind::PageNotMapped,
            (false, true) => FaultKind::PageDenied,
            (true, false) => FaultKind::InterruptBlocked,
            (true, true) => FaultKind::Other,
        }
    } else {
        FaultKind::Other
    };
    Fault {
        source_id: e.device,
        interrupt,
        address: e.address,
        interrupt_index: None,
        // RW is meaningful only for a present page (§2.5.3).
        write: (e.present() && !interrupt).then_some(e.write()),
        kind,
        detail: super::Detail::Amd(e),
    }
}

fn with<T>(f: impl FnOnce(&mut AmdVi) -> Result<T, &'static str>) -> Result<T, &'static str> {
    crate::cpu::without_interrupts(|| f(AMD.lock().as_mut().ok_or("no IOMMU")?))
}

pub(super) fn translating() -> bool {
    crate::cpu::without_interrupts(|| AMD.lock().as_ref().is_some_and(|s| !s.units.is_empty()))
}

pub(super) fn attached(d: Domain, at: Address) -> bool {
    crate::cpu::without_interrupts(|| {
        AMD.lock()
            .as_ref()
            .is_some_and(|s| s.domains.get(d.0).is_some_and(|dom| dom.at == at))
    })
}

/// The physical range of every AMD IOMMU's registers the IVRS (`table`) names, for
/// `iommu::register_ranges`.
pub(super) fn register_ranges(table: &'static [u8]) -> Result<Vec<(u64, u64)>, &'static str> {
    let ivrs = ivrs::parse(table).map_err(|_| "the IVRS is invalid")?;
    Ok(ivrs.units().map(|u| (u.base, reg::SPAN)).collect())
}

/// The AMD IOMMUs' PCI functions on segment 0, named by the IVRS (see `iommu::own_functions`).
pub(super) fn own_functions(rsdp: u64) -> Vec<Address> {
    let Ok(table) = crate::firmware::acpi_table(rsdp, b"IVRS") else {
        return Vec::new();
    };
    let Ok(ivrs) = ivrs::parse(table) else {
        return Vec::new();
    };
    ivrs.units()
        .filter(|u| u.segment == 0)
        .map(|u| address(u.device_id))
        .collect()
}

pub(super) fn attach(at: Address) -> Result<Domain, &'static str> {
    with(|s| s.attach(at))
}

pub(super) fn map(d: Domain, phys: u64) -> Result<u64, &'static str> {
    with(|s| s.map(d, phys))
}

pub(super) fn unmap(d: Domain, iova: u64) -> Result<u64, &'static str> {
    with(|s| s.unmap(d, iova))
}

pub(super) fn translate(d: Domain, iova: u64) -> Option<(u64, Access)> {
    crate::cpu::without_interrupts(|| {
        let g = AMD.lock();
        let dom = g.as_ref()?.domains.get(d.0)?;
        let mut tables = Tables {
            coherent: true,
            made: 0,
        };
        amdvi::translate(&mut tables, dom.top, LEVELS, iova)
    })
}

pub(super) fn msi_message(at: Address, apic: u32, vector: u8) -> Result<MsiMessage, &'static str> {
    with(|s| s.route(at, apic, vector))
}

pub(super) fn route(at: Address, m: MsiMessage) -> Option<Route> {
    crate::cpu::without_interrupts(|| AMD.lock().as_ref()?.lookup(at, m))
}

pub(super) fn reserve_blocked_interrupt(at: Address) -> Result<(MsiMessage, u16), &'static str> {
    with(|s| {
        let (ti, index) = s.alloc_irte(at)?;
        s.write_irte(ti, index, None)?;
        Ok((
            amdvi::remapped_msi(index).ok_or("no fixed-type index left")?,
            index,
        ))
    })
}

pub(super) fn release_interrupt(at: Address, index: u16) -> Result<(), &'static str> {
    with(|s| {
        let (ui, c) = s.resolve(at)?;
        let ti = s
            .interrupts
            .iter()
            .position(|t| t.unit == ui && t.requester == c.requester)
            .ok_or("the device has no interrupt remapping table")?;
        s.write_irte(ti, index, None)?;
        if let Some(t) = s.interrupts.get_mut(ti) {
            t.used.clear(usize::from(index));
        }
        Ok(())
    })
}

pub(super) fn take_faults() -> (Vec<Fault>, bool) {
    crate::cpu::without_interrupts(|| {
        let mut g = AMD.lock();
        let Some(s) = g.as_mut() else {
            return (Vec::new(), false);
        };
        let mut all = Vec::new();
        let mut lost = false;
        for u in &mut s.units {
            let (events, o) = u.take_events();
            all.extend(events.into_iter().map(fault));
            lost |= o;
        }
        (all, lost)
    })
}

pub(super) fn domains() -> (Vec<DomainInfo>, usize) {
    crate::cpu::without_interrupts(|| {
        let g = AMD.lock();
        let Some(s) = g.as_ref() else {
            return (Vec::new(), 0);
        };
        let list = s
            .domains
            .iter()
            .map(|d| DomainInfo {
                at: d.at,
                id: d.id,
                mapped: d.mapped,
                mapped_ever: d.mapped_ever,
                tables: d.tables,
            })
            .collect();
        let irtes = s
            .interrupts
            .iter()
            .map(|t| t.used.len() - t.used.free())
            .sum();
        (list, irtes)
    })
}

/// The DeviceID `at`'s requests carry (its alias, where the IVRS names one).
pub(super) fn source_id(at: Address) -> u16 {
    crate::cpu::without_interrupts(|| {
        AMD.lock()
            .as_ref()
            .and_then(|s| s.resolve(at).ok())
            .map_or_else(|| device_id(at), |(_, c)| c.requester)
    })
}
