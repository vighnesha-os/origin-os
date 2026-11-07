//! PCI Express enumeration (M5 groundwork; CLAUDE.md §32: host bridge discovery, PCIe
//! enumeration, configuration access, BAR mapping).
//!
//! 1. The ACPI MCFG names the ECAM windows; Origin uses segment 0's.
//! 2. The window must not overlap RAM in the boot memory map. It is mapped uncached in the
//!    MMIO window, which needs every processor's PAT to hold uncached at entry 3.
//! 3. Every function on the window's buses is listed (`framekernel::pci::enumerate`), and
//!    every BAR is sized with decoding off and then restored. The proof reads each BAR and
//!    the command register back afterwards.
//!
//! Configuration accesses are 32-bit volatile loads and stores into the window. Devices
//! are only listed here; drivers bind to them later (`nvme`). There is no IOMMU yet
//! (CLAUDE.md §32 VT-d and AMD-Vi are later steps), so a device's DMA is not yet confined.

use alloc::vec::Vec;
use core::fmt::{self, Write};
use core::ptr;

use framekernel::pci::{self, Address, Bar, BarKind, ConfigSpace, Ecam, Function};
use origin_boot_info::{MemoryRegion, RegionKind};
use origin_sync::SpinLock;

use crate::console::Console;

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
static DEVICES: SpinLock<Vec<Device>> = SpinLock::new(Vec::new());

/// Most functions the kernel lists.
const MAX_FUNCTIONS: usize = 256;

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

/// Finds, maps and enumerates segment 0's ECAM window and reports every function. Returns
/// whether every step and check passed.
pub(crate) fn init(com1: &mut Console, rsdp: u64, regions: &[MemoryRegion], cpus: usize) -> bool {
    let pat = crate::cpu::check_pat();
    let ready = crate::cpu::pat_ready();
    if ready != cpus as u64 {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: PAT entry 3 is uncached on only {ready} of {cpus} processors (IA32_PAT {pat:#x}): PCIe failed"
        );
        return false;
    }
    let mcfg = match crate::firmware::acpi_table(rsdp, b"MCFG") {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(com1, "origin-kernel: PCIe: no MCFG ({e}): PCIe failed");
            return false;
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
            return false;
        }
    };
    let Some(ecam) = allocations
        .iter()
        .take(count)
        .find(|e| e.segment == 0)
        .copied()
    else {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: the MCFG has no segment 0: PCIe failed"
        );
        return false;
    };
    let Some((phys, len)) = ecam.window() else {
        return false;
    };
    let overlaps_ram = regions.iter().any(|r| {
        let end = r.base.saturating_add(r.pages.saturating_mul(4096));
        is_ram(r.kind) && r.base < phys + len && phys < end
    });
    if overlaps_ram {
        let _ = writeln!(
            com1,
            "origin-kernel: PCIe: the ECAM window at {phys:#x} overlaps RAM: PCIe failed"
        );
        return false;
    }
    let virt = match crate::vm::map_mmio(phys, len) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: PCIe: cannot map the ECAM window: {e:?}: PCIe failed"
            );
            return false;
        }
    };
    let leaf = crate::vm::translate(virt);
    let uncached =
        leaf.is_some_and(|t| t.uncached && t.writable && !t.executable && t.phys == phys);
    let _ = writeln!(
        com1,
        "origin-kernel: PCIe: ECAM for segment 0, buses {}-{}, at {phys:#x} ({}), mapped at {virt:#x}: uncached {uncached}, {} pages; IA32_PAT {pat:#x} on {ready} processors{}",
        ecam.start_bus,
        ecam.end_bus,
        Size(len),
        Size(leaf.map_or(0, |t| t.size)),
        if uncached { "" } else { ": PCIe failed" }
    );
    if !uncached {
        return false;
    }
    let mut space = EcamSpace { ecam, virt };
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
        "origin-kernel: PCIe: {n} functions on {} buses; every BAR sized with decoding off and restored: {restored}{}",
        buses.len(),
        if restored { "" } else { ": PCIe failed" }
    );
    crate::cpu::without_interrupts(|| {
        *DEVICES.lock() = devices;
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

/// The first listed function of this class, subclass and programming interface.
pub(crate) fn find(class: u8, subclass: u8, prog_if: u8) -> Option<Device> {
    crate::cpu::without_interrupts(|| {
        DEVICES.lock().iter().copied().find(|d| {
            (d.function.class, d.function.subclass, d.function.prog_if)
                == (class, subclass, prog_if)
        })
    })
}

/// Sets bits in a function's command register (for example memory decoding and bus
/// mastering for a driver that binds to it); returns the new value.
pub(crate) fn enable(at: Address, bits: u16) -> Option<u16> {
    crate::cpu::without_interrupts(|| {
        let mut guard = ECAM.lock();
        let space = guard.as_mut()?;
        let command = space.read32(at, pci::register::COMMAND) & 0xFFFF;
        // The status half is written as zeros: its bits clear when written as one.
        space.write32(at, pci::register::COMMAND, command | u32::from(bits));
        Some((space.read32(at, pci::register::COMMAND) & 0xFFFF) as u16)
    })
}
