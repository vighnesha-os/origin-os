//! PCI Express (M5; CLAUDE.md §32: host bridge discovery, enumeration, configuration access,
//! BAR mapping) and who may master the bus (§58; threat model T4; ADR-0007 I12). In boot
//! order:
//!
//! 1. [`quiesce`], as soon as the kernel can map configuration space: the ACPI MCFG names
//!    the ECAM windows, each of which must not overlap RAM and is mapped uncached in the
//!    MMIO window. On every function of every window Bus Master Enable is cleared and read
//!    back (`framekernel::pci::clear_bus_master`), so the firmware's still-running devices
//!    stop writing memory and no later than this their posted writes are out. The IOMMUs'
//!    own functions, which no domain can confine, are left alone.
//! 2. [`settle`], once the TSC is calibrated: at most 100 ms for functions that report
//!    Transactions Pending, for all of them together, so no device can add its own delay.
//! 3. [`init`], after the application processors are up: every processor's PAT checked, every
//!    function listed and every BAR sized with decoding off and then restored.
//! 4. [`check_quiet`], before any driver binds: nothing has turned bus mastering back on.
//! 5. A driver attaches its device to a domain, and only then turns bus mastering on, through
//!    `dma::DeviceDma::enable_bus_master` ([`set_bus_master`]).
//! 6. [`prove_bus_mastering`], after the drivers ran: bus mastering is on exactly for the
//!    attached functions and the bridges above them.
//!
//! Configuration accesses are 32-bit volatile loads and stores into the window.

use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::ops::ControlFlow;
use core::ptr;

use framekernel::pci::{self, Address, Bar, BarKind, ConfigSpace, Ecam, Function};
use origin_boot_info::{MemoryRegion, RegionKind};
use origin_sync::{RwLock, SpinLock};

use crate::console::Console;
use crate::cpu::CpuIndex;

/// Segment 0's ECAM window, mapped.
struct EcamSpace {
    ecam: Ecam,
    virt: u64,
}

impl ConfigSpace for EcamSpace {
    fn read32(&mut self, at: Address, register: u16) -> u32 {
        let Some(offset) = self.ecam.offset(at, register) else {
            return u32::MAX;
        };
        let p = ptr::with_exposed_provenance::<u32>((self.virt + offset) as usize);
        // SAFETY: `offset` lies inside the window (`Ecam::offset` checked the bus, device,
        // function and register), which is mapped uncached and read/write at `virt`, and
        // is aligned to 4. A configuration read has no effect on memory.
        unsafe { p.read_volatile() }
    }

    fn write32(&mut self, at: Address, register: u16, value: u32) {
        let Some(offset) = self.ecam.offset(at, register) else {
            return;
        };
        let p = ptr::with_exposed_provenance_mut::<u32>((self.virt + offset) as usize);
        // SAFETY: as for `read32`. A configuration write reaches only the device's
        // configuration registers. The writes this kernel makes are BAR sizing (with
        // decoding off, so the all-ones probe address is never decoded, and each BAR is
        // restored) and command-register bits it owns.
        unsafe { p.write_volatile(value) };
    }
}

/// One function and its BARs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Device {
    pub(crate) function: Function,
    pub(crate) bars: [Option<Bar>; 6],
}

static ECAM: SpinLock<Option<EcamSpace>> = SpinLock::new(None);
/// The enumerated devices: written once at init, read by `find` (read-mostly, so an
/// `RwLock`).
static DEVICES: RwLock<Vec<Device>> = RwLock::new(Vec::new());

/// Most functions the kernel lists.
const MAX_FUNCTIONS: usize = 256;
/// How long [`settle`] waits for Transactions Pending to clear, over all functions together.
const SETTLE_MS: u64 = 100;

/// What [`quiesce`] did, for the later re-reads: every function it visited, on every window.
pub(crate) struct Quiesce {
    functions: Vec<(u16, Function)>,
    /// The windows of segments other than 0 (segment 0's is [`ECAM`]): their functions are
    /// quiesced and re-read, but no driver binds them.
    others: Vec<EcamSpace>,
    /// The IOMMUs' own functions, on segment 0, each with what the firmware left its Bus
    /// Master Enable reading.
    own: Vec<(Address, bool)>,
    /// The TSC when the clearing began and ended, reported once the TSC rate is known.
    started: u64,
    done: u64,
}

/// A function in a report: `00:02.0`, or `0001:00:02.0` off segment 0.
struct Named(u16, Address);

impl fmt::Display for Named {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 != 0 {
            write!(f, "{:04x}:", self.0)?;
        }
        write!(f, "{}", self.1)
    }
}

/// Several functions in a report, or `none`.
struct Names<'a>(&'a [(u16, Address)]);

impl fmt::Display for Names<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        for (i, &(segment, at)) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}", Named(segment, at))?;
        }
        Ok(())
    }
}

/// Runs `f` on the configuration space of `segment`.
fn with_space<T>(
    others: &mut [EcamSpace],
    segment: u16,
    f: impl FnOnce(&mut EcamSpace) -> T,
) -> Option<T> {
    if segment == 0 {
        crate::cpu::without_interrupts(|| ECAM.lock().as_mut().map(f))
    } else {
        others.iter_mut().find(|s| s.ecam.segment == segment).map(f)
    }
}

/// Region kinds that are RAM (or hold firmware data the kernel reads), which an ECAM
/// window must never overlap.
fn is_ram(kind: u32) -> bool {
    kind != RegionKind::Reserved as u32 && kind != RegionKind::Mmio as u32
}

/// A size for the report: `32 B`, `4 KiB`, `16 MiB`.
struct Size(u64);

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (value, unit) = match self.0 {
            s if s >= 1 << 30 && s.is_multiple_of(1 << 30) => (s >> 30, "GiB"),
            s if s >= 1 << 20 && s.is_multiple_of(1 << 20) => (s >> 20, "MiB"),
            s if s >= 1 << 10 && s.is_multiple_of(1 << 10) => (s >> 10, "KiB"),
            s => (s, "B"),
        };
        write!(f, "{value} {unit}")
    }
}

/// Maps the ECAM window `ecam` uncached, having checked that it overlaps no RAM, and reports
/// it. `None` (reported) if it cannot be used.
fn map_window(
    com1: &mut Console,
    ecam: Ecam,
    regions: &[MemoryRegion],
    pat: u64,
) -> Option<EcamSpace> {
    let (phys, len) = ecam.window()?;
    let overlaps_ram = regions.iter().any(|r| {
        let end = r.base.saturating_add(r.pages.saturating_mul(4096));
        is_ram(r.kind) && r.base < phys + len && phys < end
    });
    if overlaps_ram {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: the ECAM window at {phys:#x} overlaps RAM: PCIe failed"
        );
        return None;
    }
    let virt = match crate::vm::map_mmio(phys, len) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: PCIe: cannot map the ECAM window: {e:?}: PCIe failed"
            );
            return None;
        }
    };
    let leaf = crate::vm::translate(virt);
    let uncached =
        leaf.is_some_and(|t| t.uncached && t.writable && !t.executable && t.phys == phys);
    let _ = writeln!(
        com1,
        "origin-kernel: PCIe: ECAM for segment {}, buses {}-{}, at {phys:#x} ({}), mapped at {virt:#x}: uncached {uncached}, {} pages; IA32_PAT {pat:#x} on the boot processor{}",
        ecam.segment,
        ecam.start_bus,
        ecam.end_bus,
        Size(len),
        Size(leaf.map_or(0, |t| t.size)),
        if uncached { "" } else { ": PCIe failed" }
    );
    uncached.then_some(EcamSpace { ecam, virt })
}

/// Stops the firmware's devices: maps every ECAM window and clears Bus Master Enable on every
/// function except the IOMMUs' own (`own`, which no domain can confine). Called as soon as the
/// kernel's page tables and heap exist, on the boot processor alone. `None` (reported) if
/// segment 0's window cannot be used; the later PCIe steps then do not run.
pub(crate) fn quiesce(
    com1: &mut Console,
    rsdp: u64,
    regions: &[MemoryRegion],
    own: &[Address],
) -> Option<Quiesce> {
    let pat = crate::cpu::check_pat();
    if crate::cpu::pat_ready(&[crate::cpu::this_cpu()]) != 1 {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: PAT entry 3 is not uncached on the boot processor (IA32_PAT {pat:#x}): PCIe failed"
        );
        return None;
    }
    let mcfg = match crate::firmware::acpi_table(rsdp, b"MCFG") {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: PCIe: no MCFG ({e}): PCIe failed");
            return None;
        }
    };
    let mut allocations = [Ecam::default(); 8];
    let count = match pci::mcfg_allocations(mcfg, &mut allocations) {
        Ok(n) => n,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: PCIe: invalid MCFG: {e:?}: PCIe failed"
            );
            return None;
        }
    };
    let started = crate::cpu::rdtsc();
    let mut q = Quiesce {
        functions: Vec::new(),
        others: Vec::new(),
        own: Vec::new(),
        started,
        done: started,
    };
    let (mut left_on, mut refused) = (Vec::new(), Vec::new());
    let mut segment_zero = false;
    for ecam in allocations.iter().take(count).copied() {
        let Some(mut space) = map_window(com1, ecam, regions, pat) else {
            continue;
        };
        let _ = pci::for_each_function(&mut space, ecam.buses(), |space, f| {
            if ecam.segment == 0 && own.contains(&f.at) {
                q.own.push((f.at, pci::bus_master_on(space, f.at)));
            } else {
                let outcome = pci::clear_bus_master(space, f.at);
                if outcome.was_on {
                    left_on.push((ecam.segment, f.at));
                }
                if outcome.still_on {
                    refused.push((ecam.segment, f.at));
                }
            }
            q.functions.push((ecam.segment, f));
            ControlFlow::Continue(())
        });
        if ecam.segment == 0 {
            segment_zero = true;
            crate::cpu::without_interrupts(|| *ECAM.lock() = Some(space));
        } else {
            q.others.push(space);
        }
    }
    q.done = crate::cpu::rdtsc();
    if !segment_zero {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: the MCFG has no usable segment 0: PCIe failed"
        );
        return None;
    }
    let cleared = q.functions.len() - q.own.len();
    let _ = write!(
        com1,
        "origin-kernel: PCIe: bus mastering: the firmware left it on for {} of {cleared} functions ({}); every function reads back clear: {}",
        left_on.len(),
        Names(&left_on),
        refused.is_empty()
    );
    for &(at, on) in &q.own {
        let _ = write!(
            com1,
            "; the IOMMU's own function {at} left as the firmware set it (bus master {on})"
        );
    }
    if refused.is_empty() {
        let _ = writeln!(com1);
    } else {
        let _ = writeln!(
            com1,
            "; these ignored the write: {}: PCIe failed",
            Names(&refused)
        );
    }
    Some(q)
}

/// Waits for functions that report Transactions Pending to finish, at most [`SETTLE_MS`]
/// over all of them, and reports what the quiesce cost. Returns whether none is left.
pub(crate) fn settle(com1: &mut Console, q: &mut Quiesce, tsc_hz: u64) -> bool {
    let Quiesce {
        functions,
        others,
        own,
        started,
        done,
    } = q;
    // The IOMMUs' own functions keep their bus mastering, so nothing here applies to them.
    let cleared: Vec<(u16, Function)> = functions
        .iter()
        .copied()
        .filter(|&(segment, f)| !(segment == 0 && own.iter().any(|&(at, _)| at == f.at)))
        .collect();
    let express: Vec<(u16, Address, u16)> = cleared
        .iter()
        .filter_map(|&(segment, f)| {
            let capability = with_space(others, segment, |space| {
                pci::find_capability(space, f.at, pci::capability::PCI_EXPRESS)
            })??;
            Some((segment, f.at, capability))
        })
        .collect();
    let begin = crate::cpu::rdtsc();
    let limit = tsc_hz.saturating_mul(SETTLE_MS) / 1000;
    let (pending, waited) = loop {
        let pending: Vec<(u16, Address)> = express
            .iter()
            .filter(|&&(segment, at, capability)| {
                with_space(others, segment, |space| {
                    pci::transactions_pending(space, at, capability)
                })
                .unwrap_or(false)
            })
            .map(|&(segment, at, _)| (segment, at))
            .collect();
        let waited = crate::cpu::rdtsc().wrapping_sub(begin);
        if pending.is_empty() || waited >= limit {
            break (pending, waited);
        }
        core::hint::spin_loop();
    };
    let us = |ticks: u64| ticks.saturating_mul(1_000_000) / tsc_hz.max(1);
    let since_entry = done.saturating_sub(crate::time::entry_tsc());
    let _ = write!(
        com1,
        "origin-kernel: PCIe: bus mastering: {} of {} functions have a PCI Express capability; Transactions Pending clear on {} of them after waiting {} us (limit {SETTLE_MS} ms); clearing took {} us and finished {} us after kernel entry",
        express.len(),
        cleared.len(),
        express.len() - pending.len(),
        us(waited),
        us(done.saturating_sub(*started)),
        us(since_entry),
    );
    if pending.is_empty() {
        let _ = writeln!(com1);
    } else {
        let _ = writeln!(com1, "; still pending: {}: PCIe failed", Names(&pending));
    }
    pending.is_empty()
}

/// The functions that have Bus Master Enable set, the IOMMUs' own excepted.
fn bus_masters(q: &mut Quiesce) -> Vec<(u16, Function)> {
    let Quiesce {
        functions,
        others,
        own,
        ..
    } = q;
    functions
        .iter()
        .filter(|&&(segment, f)| {
            let mine = segment == 0 && own.iter().any(|&(at, _)| at == f.at);
            !mine
                && with_space(others, segment, |space| pci::bus_master_on(space, f.at))
                    .unwrap_or(false)
        })
        .copied()
        .collect()
}

/// Re-reads every function before any driver binds: nothing has turned bus mastering back
/// on since [`quiesce`]. Returns whether that held.
pub(crate) fn check_quiet(com1: &mut Console, q: &mut Quiesce) -> bool {
    let on: Vec<(u16, Address)> = bus_masters(q)
        .iter()
        .map(|&(segment, f)| (segment, f.at))
        .collect();
    let n = q.functions.len() - q.own.len();
    let quiet = on.is_empty();
    let _ = write!(
        com1,
        "origin-kernel: PCIe: bus mastering: off on all {n} functions before any driver attached: {quiet}"
    );
    if quiet {
        let _ = writeln!(com1);
    } else {
        let _ = writeln!(com1, " (on for {}): PCIe failed", Names(&on));
    }
    quiet
}

/// Reads every function back after the drivers ran: with the IOMMU translating, bus
/// mastering is on exactly for the functions attached to a domain and the bridges above
/// them. Returns whether that held.
pub(crate) fn prove_bus_mastering(com1: &mut Console, q: &mut Quiesce) -> bool {
    let translating = crate::iommu::translating();
    let (domains, _) = crate::iommu::domains();
    let attached: Vec<Address> = domains.iter().map(|d| d.at).collect();
    let on = bus_masters(q);
    let mut wrong = Vec::new();
    let mut bridges = 0usize;
    {
        let Quiesce {
            functions,
            others,
            own,
            ..
        } = q;
        // The IOMMUs' own functions still read as the firmware left them.
        for &(at, on) in own.iter() {
            if with_space(others, 0, |space| pci::bus_master_on(space, at)) != Some(on) {
                wrong.push((0, at));
            }
        }
        for &(segment, f) in functions.iter() {
            if segment == 0 && own.iter().any(|&(at, _)| at == f.at) {
                continue;
            }
            let reads_on = on.iter().any(|&(s, g)| s == segment && g.at == f.at);
            let Some(buses) = with_space(others, segment, |space| pci::bridge_buses(space, &f))
            else {
                continue;
            };
            let allowed = segment == 0 && pci::bus_master_allowed(&f, buses.as_ref(), &attached);
            bridges += usize::from(reads_on && f.layout == 1);
            if reads_on != allowed {
                wrong.push((segment, f.at));
            }
        }
    }
    let total = q.functions.len() - q.own.len();
    let _ = write!(
        com1,
        "origin-kernel: PCIe: bus mastering: on for exactly the {} attached function(s) (",
        attached.len()
    );
    for (i, d) in domains.iter().enumerate() {
        let _ = write!(
            com1,
            "{}{} domain {}",
            if i > 0 { ", " } else { "" },
            d.at,
            d.id
        );
    }
    let ok = translating && wrong.is_empty();
    let _ = write!(
        com1,
        ") and the {bridges} bridge(s) above them, off on the other {}; IOMMU translating {translating}; consistent {ok}",
        total.saturating_sub(attached.len() + bridges)
    );
    if ok {
        let _ = writeln!(com1);
    } else {
        let _ = writeln!(com1, "; wrong: {}: PCIe failed", Names(&wrong));
    }
    ok
}

/// Lists segment 0's functions with their BARs sized, once every processor is up. Returns
/// whether every step and check passed; [`quiesce`] must have succeeded.
pub(crate) fn init(com1: &mut Console, cpus: &[CpuIndex]) -> bool {
    let pat = crate::cpu::check_pat();
    let ready = crate::cpu::pat_ready(cpus);
    if ready != cpus.len() {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: PAT entry 3 is uncached on only {ready} of {} processors (IA32_PAT {pat:#x}): PCIe failed",
            cpus.len()
        );
        return false;
    }
    let Some(mut space) = crate::cpu::without_interrupts(|| ECAM.lock().take()) else {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: segment 0 was not mapped: PCIe failed"
        );
        return false;
    };
    let ecam = space.ecam;
    let mut functions = [Function::default(); MAX_FUNCTIONS];
    let n = match pci::enumerate(&mut space, ecam.buses(), &mut functions) {
        Ok(n) => n,
        Err(n) => {
            let _ = writeln!(
                com1,
                "origin-kernel: PCIe: more than {n} functions; listing the first {n}"
            );
            n
        }
    };
    let mut restored = true;
    let mut devices = Vec::with_capacity(n);
    for f in functions.iter().take(n) {
        let snapshot = |space: &mut EcamSpace| {
            let mut regs = [0u32; 7];
            regs[0] = space.read32(f.at, pci::register::COMMAND) & 0xFFFF;
            for (i, r) in regs
                .iter_mut()
                .skip(1)
                .enumerate()
                .take(usize::from(f.bar_count()))
            {
                *r = space.read32(f.at, pci::register::BAR0 + 4 * i as u16);
            }
            regs
        };
        let before = snapshot(&mut space);
        let bars = pci::size_bars(&mut space, f);
        restored &= snapshot(&mut space) == before;
        report(com1, f, &bars);
        devices.push(Device { function: *f, bars });
    }
    let buses: Vec<u8> = {
        let mut b: Vec<u8> = devices.iter().map(|d| d.function.at.bus).collect();
        b.dedup();
        b
    };
    let _ = writeln!(
        com1,
        "origin-kernel: PCIe: {n} functions on {} buses; every BAR sized with decoding off and restored: {restored}; PAT entry 3 uncached on {ready} processors (IA32_PAT {pat:#x}){}",
        buses.len(),
        if restored { "" } else { ": PCIe failed" }
    );
    crate::cpu::without_interrupts(|| {
        *DEVICES.write() = devices;
        *ECAM.lock() = Some(space);
    });
    restored
}

fn report(com1: &mut Console, f: &Function, bars: &[Option<Bar>; 6]) {
    let _ = write!(
        com1,
        "origin-kernel: PCIe: {} {:04x}:{:04x} class {:02x}:{:02x}:{:02x}",
        f.at, f.vendor, f.device, f.class, f.subclass, f.prog_if
    );
    if let Some(name) = pci::class_name(f.class, f.subclass, f.prog_if) {
        let _ = write!(com1, " {name}");
    }
    for bar in bars.iter().flatten() {
        let kind = match bar.kind {
            BarKind::Io => "I/O",
            BarKind::Memory32 => "mem32",
            BarKind::Memory64 => "mem64",
        };
        let _ = write!(
            com1,
            "; BAR{} {kind}{} {} at {:#x}",
            bar.index,
            if bar.prefetchable {
                " prefetchable"
            } else {
                ""
            },
            Size(bar.size),
            bar.address
        );
    }
    let _ = writeln!(com1);
}

/// The function's MSI-X capability, if it has one.
pub(crate) fn msix(at: Address) -> Option<pci::Msix> {
    crate::cpu::without_interrupts(|| {
        let mut guard = ECAM.lock();
        pci::msix(guard.as_mut()?, at)
    })
}

/// Turns MSI-X on for the function, with the function mask clear: only entries the driver
/// unmasked can then fire. Returns the message control word read back.
pub(crate) fn enable_msix(at: Address, msix: &pci::Msix) -> Option<u32> {
    crate::cpu::without_interrupts(|| {
        let mut guard = ECAM.lock();
        let space = guard.as_mut()?;
        let dword = space.read32(at, msix.capability);
        let control = ((dword >> 16) | pci::MSIX_ENABLE) & !pci::MSIX_FUNCTION_MASK;
        // The low half (ID and next pointer) is read-only.
        space.write32(at, msix.capability, (control << 16) | (dword & 0xFFFF));
        Some(space.read32(at, msix.capability) >> 16)
    })
}

/// How many PCIe functions were enumerated.
pub(crate) fn device_count() -> usize {
    crate::cpu::without_interrupts(|| DEVICES.read().len())
}

/// Reads a configuration-space dword (for a driver walking its capability list).
pub(crate) fn read_config32(at: Address, register: u16) -> Option<u32> {
    crate::cpu::without_interrupts(|| ECAM.lock().as_mut().map(|s| s.read32(at, register)))
}

/// The first listed function of this class, subclass and programming interface.
pub(crate) fn find(class: u8, subclass: u8, prog_if: u8) -> Option<Device> {
    crate::cpu::without_interrupts(|| {
        DEVICES.read().iter().copied().find(|d| {
            (d.function.class, d.function.subclass, d.function.prog_if)
                == (class, subclass, prog_if)
        })
    })
}

/// Sets `bit` in a function's command register, reads it back and returns the new value.
fn set_command_bit(space: &mut EcamSpace, at: Address, bit: u16) -> Option<u16> {
    let command = space.read32(at, pci::register::COMMAND) & 0xFFFF;
    // The status half is written as zeros: its bits clear when written as one.
    space.write32(at, pci::register::COMMAND, command | u32::from(bit));
    let after = (space.read32(at, pci::register::COMMAND) & 0xFFFF) as u16;
    (after & bit != 0).then_some(after)
}

/// Turns memory decoding on for a driver that binds to the function; returns the new value
/// of its command register. Bus mastering is separate: [`set_bus_master`].
pub(crate) fn enable_memory(at: Address) -> Option<u16> {
    crate::cpu::without_interrupts(|| {
        let mut guard = ECAM.lock();
        set_command_bit(guard.as_mut()?, at, pci::MEMORY_SPACE)
    })
}

/// Turns bus mastering on for the function at `at` and for the bridges above it, nearest the
/// root first, so its requests can reach memory. Returns the function's command register.
/// Refuses if the function is already mastering: something turned it on before its driver
/// attached it. Only `dma::DeviceDma::enable_bus_master` calls this, after attaching.
pub(crate) fn set_bus_master(at: Address) -> Result<u16, &'static str> {
    crate::cpu::without_interrupts(|| {
        let devices = DEVICES.read();
        let mut guard = ECAM.lock();
        let space = guard.as_mut().ok_or("no configuration access")?;
        if !devices.iter().any(|d| d.function.at == at) {
            return Err("the function is not listed");
        }
        if pci::bus_master_on(space, at) {
            return Err("bus mastering was already on before the driver attached the device");
        }
        // The list is in bus order, so a bridge comes before the bridges and functions below
        // it (firmware numbers buses downstream).
        for d in devices.iter() {
            if pci::bridge_buses(space, &d.function).is_some_and(|b| b.contains(&at.bus)) {
                set_command_bit(space, d.function.at, pci::BUS_MASTER)
                    .ok_or("a bridge above the device kept bus mastering off")?;
            }
        }
        set_command_bit(space, at, pci::BUS_MASTER).ok_or("bus mastering did not turn on")
    })
}
