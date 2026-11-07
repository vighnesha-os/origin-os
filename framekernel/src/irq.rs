//! I/O APIC interrupt routing (CLAUDE.md §24 interrupt routing, §31 interrupt management):
//! discover the I/O APIC from the ACPI MADT, and route a legacy IRQ through it to a local
//! APIC vector. Proven by routing the 8254 PIT's IRQ0 and taking periodic interrupts.
//!
//! The register encodings are `framekernel::ioapic`; this module does the indirect MMIO
//! (IOREGSEL/IOWIN) and the PIT programming. Only the boot processor runs this, with the
//! local APIC already in x2APIC mode and the 8259 PICs masked (`timer::init`).

use core::fmt::Write;

use framekernel::acpi::{self, IoApic, SourceOverride};
use framekernel::apic;
use framekernel::ioapic;

use crate::console::Console;
use crate::cpu::{self, rdtsc};
use crate::outb;

/// The PIT's command port and channel-0 data port.
const PIT_CHANNEL0: u16 = 0x40;
/// Channel 0, access lo/hi byte, mode 2 (rate generator), binary.
const PIT_CHANNEL0_MODE2: u8 = 0x34;
/// Channel 0, access lo/hi byte, mode 0 (one-shot): used to stop the rate generator.
const PIT_CHANNEL0_MODE0: u8 = 0x30;
/// The routed rate, and how many interrupts the proof waits for.
const HZ: u64 = 100;
const PROOF_TICKS: u64 = 20;

/// The mapped I/O APIC: its MMIO virtual base and GSI base.
struct MappedIoApic {
    virt: u64,
    gsi_base: u32,
}

impl MappedIoApic {
    fn read(&self, index: u32) -> u32 {
        let sel =
            core::ptr::with_exposed_provenance_mut::<u32>((self.virt + ioapic::IOREGSEL) as usize);
        let win = core::ptr::with_exposed_provenance::<u32>((self.virt + ioapic::IOWIN) as usize);
        // SAFETY: `virt` maps the I/O APIC's registers uncached; writing IOREGSEL selects a
        // register, with no memory effect. Only the boot processor touches these.
        unsafe { sel.write_volatile(index) };
        // SAFETY: reading IOWIN returns the selected register, with no memory effect.
        unsafe { win.read_volatile() }
    }

    fn write(&self, index: u32, value: u32) {
        let sel =
            core::ptr::with_exposed_provenance_mut::<u32>((self.virt + ioapic::IOREGSEL) as usize);
        let win =
            core::ptr::with_exposed_provenance_mut::<u32>((self.virt + ioapic::IOWIN) as usize);
        // SAFETY: as for `read`; selecting a register has no memory effect.
        unsafe { sel.write_volatile(index) };
        // SAFETY: the only registers written through IOWIN here are the redirection table,
        // which routes device interrupts.
        unsafe { win.write_volatile(value) };
    }

    /// Writes redirection entry `n` (relative to this I/O APIC's GSI base): high dword
    /// first so the entry is never briefly live with the wrong destination.
    fn set_redirection(&self, n: u32, low: u32, high: u32) {
        self.write(ioapic::redirection_high(n), high);
        self.write(ioapic::redirection_low(n), low);
    }
}

/// Programs the PIT channel 0 as a `HZ` rate generator (mode 2).
fn start_pit() {
    let divisor = (apic::PIT_HZ / HZ) as u16;
    // SAFETY: CPL 0, boot processor; the PIT command port cannot affect memory.
    unsafe { outb(apic::PIT_COMMAND, PIT_CHANNEL0_MODE2) };
    // SAFETY: as above; the PIT channel-0 data port.
    unsafe { outb(PIT_CHANNEL0, (divisor & 0xFF) as u8) };
    // SAFETY: as above; the high byte of the divisor.
    unsafe { outb(PIT_CHANNEL0, (divisor >> 8) as u8) };
}

/// Stops the PIT channel 0 (mode 0, count 0: it fires once and then idles).
fn stop_pit() {
    // SAFETY: as for `start_pit`; the PIT command port.
    unsafe { outb(apic::PIT_COMMAND, PIT_CHANNEL0_MODE0) };
    // SAFETY: as above; channel-0 data, low then high byte of a zero count.
    unsafe { outb(PIT_CHANNEL0, 0) };
    // SAFETY: as above.
    unsafe { outb(PIT_CHANNEL0, 0) };
}

/// Discovers the I/O APIC, routes the PIT's IRQ0 through it to the boot processor, and
/// proves periodic interrupts arrive. `rsdp` is the ACPI RSDP, `bsp` the boot processor's
/// APIC ID, `tsc_hz` the TSC rate. Returns whether the proof passed (no I/O APIC at all is
/// reported and passes).
pub(crate) fn prove(com1: &mut Console, rsdp: u64, bsp: u32, tsc_hz: u64) -> bool {
    let Ok(madt) = crate::firmware::acpi_table(rsdp, b"APIC") else {
        let _ = writeln!(com1, "origin-kernel: I/O APIC: no MADT");
        return true;
    };
    let mut ioapics = [IoApic::default(); 4];
    let mut overrides = [SourceOverride::default(); 16];
    let (Ok(n_io), Ok(n_ov)) = (
        acpi::madt_ioapics(madt, &mut ioapics),
        acpi::madt_overrides(madt, &mut overrides),
    ) else {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: invalid MADT: I/O APIC failed"
        );
        return false;
    };
    let Some(&io) = ioapics.get(..n_io).and_then(<[_]>::first) else {
        let _ = writeln!(com1, "origin-kernel: I/O APIC: none in the MADT");
        return true;
    };
    let virt = match crate::vm::map_mmio(u64::from(io.address), 0x1000) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: I/O APIC: cannot map {:#x}: {e:?}: I/O APIC failed",
                io.address
            );
            return false;
        }
    };
    let mapped = MappedIoApic {
        virt,
        gsi_base: io.gsi_base,
    };
    let entries = ioapic::max_redirection_entry(mapped.read(ioapic::REG_VERSION)) + 1;
    // IRQ0 (the PIT) may be remapped to another GSI by an interrupt source override.
    let (gsi, flags) = acpi::gsi_for_irq(0, overrides.get(..n_ov).unwrap_or(&[]));
    let (active_low, level) = ioapic::decode_flags(flags);
    let Some(index) = gsi.checked_sub(mapped.gsi_base).filter(|&i| i < entries) else {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: GSI {gsi} is outside this I/O APIC: I/O APIC failed"
        );
        return false;
    };
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: id {}, {entries} entries at {:#x} (GSI base {}); routing the PIT's IRQ0 via GSI {gsi} to vector {:#x} on CPU {bsp}",
        io.id,
        io.address,
        io.gsi_base,
        cpu::IOAPIC_VECTOR
    );

    let before = cpu::ioapic_irqs();
    let (low, high) =
        ioapic::redirection_entry(cpu::IOAPIC_VECTOR, bsp as u8, active_low, level, false);
    mapped.set_redirection(index, low, high);
    start_pit();

    // Count interrupts, polling with interrupt windows (never halting), within two seconds.
    let start = rdtsc();
    let limit = tsc_hz.saturating_mul(2);
    while cpu::ioapic_irqs() - before < PROOF_TICKS && rdtsc().wrapping_sub(start) < limit {
        cpu::interrupt_window();
        core::hint::spin_loop();
    }
    let elapsed = rdtsc().wrapping_sub(start);
    // Mask the entry and stop the PIT: nothing schedules on this yet.
    let (masked_low, _) =
        ioapic::redirection_entry(cpu::IOAPIC_VECTOR, bsp as u8, active_low, level, true);
    mapped.set_redirection(index, masked_low, high);
    stop_pit();

    let taken = cpu::ioapic_irqs() - before;
    let micros = elapsed.saturating_mul(1_000_000) / tsc_hz.max(1);
    let ok = taken >= PROOF_TICKS;
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: {taken} PIT interrupts routed through the I/O APIC in {} ms{}",
        micros / 1000,
        if ok { "" } else { ": I/O APIC failed" }
    );
    ok
}
