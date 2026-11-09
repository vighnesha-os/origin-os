//! I/O APIC interrupt routing (CLAUDE.md §24 interrupt routing, §31 interrupt management,
//! §32 interrupt remapping): discover the I/O APIC from the ACPI MADT, and route a legacy IRQ
//! through it to a local APIC vector. Proven twice with the 8254 PIT's IRQ0:
//! - [`prove`], before the IOMMU is up: a compatibility-format redirection entry names the
//!   boot processor and the vector;
//! - [`prove_remapped`], once a VT-d unit remaps interrupts (ADR-0007 I8): the entry is in
//!   remappable format and names an IRTE, which holds the vector and the 32-bit x2APIC
//!   destination and checks the I/O APIC's source ID. With compatibility-format interrupts
//!   blocked, as they are once remapping is on, this is the only format whose interrupts
//!   arrive on hardware (QEMU passes the other through, so the proof shows the difference
//!   itself: with the IRTE not present, nothing may arrive). Under AMD-Vi the pins are not
//!   routed yet: the I/O APIC's device table entry blocks its fixed interrupts until it has
//!   an interrupt table, and [`prove_remapped`] says so.
//!
//! The register encodings are `framekernel::ioapic`; this module does the indirect MMIO
//! (IOREGSEL/IOWIN) and the PIT programming. Only the boot processor runs this, with the
//! local APIC already in x2APIC mode and the 8259 PICs masked (`timer::init`).
#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::acpi::{self, IoApic, SourceOverride};
use crate::ioapic::{self, RemappableEntry};
use crate::vtd::{Irte, SourceCheck};
use origin_frame::addr::PhysAddr;
use origin_frame::irq::{self as vectors, Action};
use origin_frame::mmio::MmioRegion;
use origin_frame::sync::SpinLock;

use crate::console::Console;
use crate::cpu::{self, CpuIndex, MAX_CPUS, rdtsc};
use crate::iommu::{Detail, Fault, Recorded};
use crate::timer;

/// The routed rate, and how many interrupts the proof waits for.
const HZ: u64 = 100;
const PROOF_TICKS: u64 = 20;
/// How long a probe watches a blocked pin: ten PIT periods.
const BLOCKED_MS: u64 = 100;
/// How long a probe lets an interrupt already on its way to a processor be taken before it
/// starts counting: half a PIT period.
const SETTLE_MS: u64 = 5;
/// Interrupts that must arrive again once a probe restores the IRTE.
const RESUME_TICKS: u64 = 5;
/// The source ID a fault record carries when the interrupt message carried none. QEMU 11.1's
/// I/O APIC sends its messages without a requester ID, and its VT-d model then records
/// 0xffff and skips the IRTE's source-ID check (`hw/intc/ioapic.c`, `ioapic_service`;
/// `hw/i386/intel_iommu.c`, `vtd_irte_get`). On hardware every message carries one: the I/O
/// APIC's, as the DMAR names it.
const NO_REQUESTER_ID: u16 = 0xFFFF;
/// Fault reasons the probes expect (VT-d chapter 7): the IRTE is not present; the
/// message's source ID failed the IRTE's check.
const REASON_NOT_PRESENT: u8 = 0x22;
const REASON_SOURCE_ID: u8 = 0x26;

/// The mapped I/O APIC: its registers and GSI base. Only the boot processor touches them.
#[derive(Clone, Copy)]
struct MappedIoApic {
    regs: MmioRegion,
    gsi_base: u32,
}

impl MappedIoApic {
    fn read(&self, index: u32) -> u32 {
        self.regs.write32(ioapic::IOREGSEL as usize, index);
        self.regs.read32(ioapic::IOWIN as usize)
    }

    fn write(&self, index: u32, value: u32) {
        self.regs.write32(ioapic::IOREGSEL as usize, index);
        self.regs.write32(ioapic::IOWIN as usize, value);
    }

    /// Writes redirection entry `n` (relative to this I/O APIC's GSI base): masked first,
    /// then the high dword, then the low one, so the entry is never live while half written
    /// (with the wrong destination, or a remappable index whose halves disagree).
    fn set_redirection(&self, n: u32, low: u32, high: u32) {
        self.write(ioapic::redirection_low(n), low | MASKED);
        self.write(ioapic::redirection_high(n), high);
        self.write(ioapic::redirection_low(n), low);
    }

    /// Reads redirection entry `n` back: `(low, high)`.
    fn redirection(&self, n: u32) -> (u32, u32) {
        (
            self.read(ioapic::redirection_low(n)),
            self.read(ioapic::redirection_high(n)),
        )
    }
}

/// A redirection entry's mask bit (low dword bit 16).
const MASKED: u32 = 1 << 16;

/// Interrupts taken on the I/O APIC's vector.
static IOAPIC_IRQS: AtomicU64 = AtomicU64::new(0);
/// Interrupts taken on the I/O APIC's vector, by the CPU index of the processor that took them.
static IOAPIC_IRQS_ON: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// How many I/O APIC-routed interrupts have been taken.
fn ioapic_irqs() -> u64 {
    IOAPIC_IRQS.load(Ordering::Relaxed)
}

/// How many I/O APIC-routed interrupts the processor with CPU index `cpu` has taken.
pub(crate) fn ioapic_irqs_on(cpu: CpuIndex) -> u64 {
    IOAPIC_IRQS_ON
        .get(cpu.get())
        .map_or(0, |n| n.load(Ordering::Relaxed))
}

/// An I/O APIC-routed interrupt: counted, and the interrupted context resumes.
fn on_interrupt() -> Action {
    IOAPIC_IRQS.fetch_add(1, Ordering::Relaxed);
    if let Some(n) = IOAPIC_IRQS_ON.get(cpu::this_cpu().get()) {
        n.fetch_add(1, Ordering::Relaxed);
    }
    // The console's boot proof prints from this handler once.
    crate::console::on_proof_interrupt();
    Action::Resume
}

/// The pin the PIT's IRQ0 reaches, as [`prove`] found it, kept for [`prove_remapped`].
#[derive(Clone, Copy)]
struct PitPin {
    io: MappedIoApic,
    /// The I/O APIC's ID: the MADT's, which is the DMAR scope's enumeration ID.
    id: u8,
    /// The redirection entry, relative to the I/O APIC's GSI base.
    entry: u32,
    gsi: u32,
    active_low: bool,
    level: bool,
}

static PIT_PIN: SpinLock<Option<PitPin>> = SpinLock::new(None);

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
    // A compatibility-format redirection entry names an 8-bit destination.
    let Ok(destination) = u8::try_from(bsp) else {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: the boot processor's APIC ID {bsp} does not fit a compatibility-format redirection entry: I/O APIC failed"
        );
        return false;
    };
    let regs = match crate::vm::map_mmio(PhysAddr::new(u64::from(io.address)), 0x1000) {
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
        regs,
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
    // Kept for the proof that routes this pin again through interrupt remapping.
    cpu::without_interrupts(|| {
        *PIT_PIN.lock() = Some(PitPin {
            io: mapped,
            id: io.id,
            entry: index,
            gsi,
            active_low,
            level,
        });
    });
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: id {}, {entries} entries at {:#x} (GSI base {}); routing the PIT's IRQ0 via GSI {gsi} to vector {:#x} on CPU {bsp}",
        io.id,
        io.address,
        io.gsi_base,
        cpu::IOAPIC_VECTOR
    );

    if vectors::register(cpu::IOAPIC_VECTOR, on_interrupt).is_err() {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: its interrupt vector is already taken: I/O APIC failed"
        );
        return false;
    }
    let before = ioapic_irqs();
    let (low, high) =
        ioapic::redirection_entry(cpu::IOAPIC_VECTOR, destination, active_low, level, false);
    mapped.set_redirection(index, low, high);
    timer::pit_channel0_start(HZ);

    // Count interrupts, polling with interrupt windows (never halting), within two seconds.
    let start = rdtsc();
    let limit = tsc_hz.saturating_mul(2);
    while ioapic_irqs() - before < PROOF_TICKS && rdtsc().wrapping_sub(start) < limit {
        cpu::interrupt_window();
        core::hint::spin_loop();
    }
    let elapsed = rdtsc().wrapping_sub(start);
    // Mask the entry and stop the PIT: nothing schedules on this yet.
    let (masked_low, _) =
        ioapic::redirection_entry(cpu::IOAPIC_VECTOR, destination, active_low, level, true);
    mapped.set_redirection(index, masked_low, high);
    timer::pit_channel0_stop();

    let taken = ioapic_irqs() - before;
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

/// Interrupts each of `cpus` has taken on the I/O APIC vector so far.
fn taken(cpus: &[CpuIndex]) -> Vec<u64> {
    cpus.iter().map(|&c| ioapic_irqs_on(c)).collect()
}

/// Interrupts taken since `before` (from [`taken`] over the same `cpus`): on `target`, and
/// on every other processor.
fn since(cpus: &[CpuIndex], before: &[u64], target: CpuIndex) -> (u64, u64) {
    let (mut on, mut elsewhere) = (0, 0);
    for (&c, &b) in cpus.iter().zip(before) {
        let n = ioapic_irqs_on(c).wrapping_sub(b);
        if c == target {
            on += n;
        } else {
            elsewhere += n;
        }
    }
    (on, elsewhere)
}

/// Lets every processor, this one included, take interrupts for `ms` milliseconds.
fn wait_ms(ms: u64, tsc_hz: u64) {
    let start = rdtsc();
    let span = tsc_hz.saturating_mul(ms) / 1000;
    crate::sched::yield_until(
        || rdtsc().wrapping_sub(start) >= span,
        tsc_hz,
        1 + ms / 1000,
    );
}

/// Waits, for at most a second, until `cpu` has taken `n` more interrupts on the I/O APIC
/// vector; returns whether it did.
fn wait_for(cpu: CpuIndex, n: u64, tsc_hz: u64) -> bool {
    let from = ioapic_irqs_on(cpu);
    let more = || ioapic_irqs_on(cpu).wrapping_sub(from) >= n;
    crate::sched::yield_until(more, tsc_hz, 1);
    more()
}

/// The VT-d fault reason of `f`: the pin proofs run only under VT-d
/// ([`crate::iommu::routes_ioapic_pins`]).
fn reason(f: &Fault) -> Option<u8> {
    match f.detail {
        Detail::Vtd(r) => Some(r.reason),
        Detail::Amd(_) => None,
    }
}

/// What a probe saw while the pin's IRTE was changed.
struct Probe {
    /// The change was written, its interrupt-entry-cache entry invalidated, and the table
    /// read back holding it.
    written: bool,
    /// Interrupts that arrived, on any processor, while the change held.
    arrived: u64,
    /// The fault the unit recorded for the pin's IRTE, if any; whether a fault was lost.
    fault: Option<Fault>,
    lost: bool,
    /// The route was restored, and [`RESUME_TICKS`] interrupts arrived on the target again.
    restored: bool,
    resumed: bool,
}

/// Rewrites `route`'s IRTE as `entry` (`None`: not present) while the PIT runs: lets an
/// interrupt already on its way be taken ([`SETTLE_MS`]), counts what arrives over
/// [`BLOCKED_MS`] and what the unit records, then restores the route and waits for
/// interrupts to arrive on `target` again.
fn probe(
    route: &crate::iommu::PinRoute,
    entry: Option<Irte>,
    cpus: &[CpuIndex],
    target: CpuIndex,
    tsc_hz: u64,
) -> Probe {
    let _ = crate::iommu::take_faults();
    let written =
        crate::iommu::rewrite_pin(route, entry).is_ok() && crate::iommu::pin_irte(route) == entry;
    wait_ms(SETTLE_MS, tsc_hz);
    let before = taken(cpus);
    wait_ms(BLOCKED_MS, tsc_hz);
    let (on, elsewhere) = since(cpus, &before, target);
    let restored = crate::iommu::rewrite_pin(route, Some(route.entry)).is_ok();
    let (faults, lost) = crate::iommu::take_faults();
    let fault = faults
        .iter()
        .find(|f| f.interrupt_index == Some(route.index))
        .copied();
    let resumed = restored && wait_for(target, RESUME_TICKS, tsc_hz);
    Probe {
        written,
        arrived: on + elsewhere,
        fault,
        lost,
        restored,
        resumed,
    }
}

/// Routes the PIT's IRQ0 again once interrupts are remapped (ADR-0007 I8), and proves it:
/// 1. **routed**: an IRTE on the unit whose DMAR scope lists the I/O APIC sends the vector
///    to a target processor (not this one, when there is another), with the pin's trigger
///    mode, checking the I/O APIC's source ID from that scope; the pin's redirection entry
///    names the IRTE in remappable format. Both read back exactly, and agree.
/// 2. **delivered**: [`PROOF_TICKS`] PIT interrupts arrive on the target and none elsewhere.
/// 3. **the IRTE not present**: for [`BLOCKED_MS`] no interrupt arrives, and the unit records
///    a fault for that index, reason 22h. This is what shows the interrupts go through the
///    IRTE: a compatibility-format entry's would still arrive under QEMU, which does not
///    block that format. The fault's source ID is the I/O APIC's — or, under QEMU, whose I/O
///    APIC sends none, [`NO_REQUESTER_ID`].
/// 4. **a wrong source ID**: the IRTE checks a source ID one function off the I/O APIC's.
///    Where the messages carry the I/O APIC's source ID (step 3 recorded it), nothing may
///    arrive and the unit records reason 26h with that source ID. Where they carry none
///    (QEMU), the unit has nothing to check: the interrupts must still arrive with nothing
///    recorded, and the line says the check cannot be shown on this platform.
///
/// After each probe the IRTE is restored and interrupts must arrive again. Then the entry is
/// masked, the PIT stopped and the IRTE released. Returns whether everything held; without
/// interrupt remapping, or without an I/O APIC, there is nothing to prove.
pub(crate) fn prove_remapped(com1: &mut Console, cpus: &[CpuIndex], tsc_hz: u64) -> bool {
    let Some(pin) = cpu::without_interrupts(|| *PIT_PIN.lock()) else {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: remapped: no I/O APIC pin was found to route"
        );
        return true;
    };
    if !crate::iommu::remapping() {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: remapped: interrupts are not remapped, so the I/O APIC keeps compatibility format"
        );
        return true;
    }
    if !crate::iommu::routes_ioapic_pins() {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: remapped: not under AMD-Vi yet: the I/O APIC's device table entry blocks its fixed interrupts until it has an interrupt table, so its pins stay masked"
        );
        return true;
    }
    let bsp = cpu::this_cpu();
    let target = cpus.iter().copied().find(|&c| c != bsp).unwrap_or(bsp);
    let mut ids = cpus.to_vec();
    if !ids.contains(&target) {
        ids.push(target);
    }
    // The IRTE's destination is the target's x2APIC ID: any 32-bit ID, since the redirection
    // entry carries only the IRTE's index (ADR-0008: CPU indexes are not APIC IDs).
    let Some(apic) = target.apic_id() else {
        let _ = writeln!(
            com1,
            "origin-kernel: I/O APIC: remapped: CPU {target} has no APIC ID to route to: I/O APIC failed"
        );
        return false;
    };
    let named = target.named();
    let trigger = if pin.level { "level" } else { "edge" };
    let polarity = if pin.active_low { "low" } else { "high" };
    // Earlier faults are cleared, so this proof's own can be recorded (QEMU has one
    // recording register, and drops a fault while it is full).
    let (stale, _) = crate::iommu::take_faults();
    let route = match crate::iommu::route_ioapic_pin(pin.id, apic, cpu::IOAPIC_VECTOR, pin.level) {
        Ok(r) => r,
        Err(why) => {
            let _ = writeln!(
                com1,
                "origin-kernel: I/O APIC: remapped: the PIT's IRQ0 (GSI {}) cannot be routed through an IRTE: {why}: I/O APIC failed",
                pin.gsi
            );
            return false;
        }
    };

    // 1. Routed: the redirection entry names the IRTE in remappable format; both read back.
    let rte = RemappableEntry {
        index: route.index,
        vector: cpu::IOAPIC_VECTOR,
        active_low: pin.active_low,
        level: pin.level,
        masked: false,
    };
    let encoded = rte.encode();
    if let Some((low, high)) = encoded {
        pin.io.set_redirection(pin.entry, low, high);
    }
    let (low, high) = pin.io.redirection(pin.entry);
    let entry_back = RemappableEntry::decode(low, high);
    let expected = Irte {
        vector: cpu::IOAPIC_VECTOR,
        destination: apic,
        source: SourceCheck::RequesterId {
            source_id: route.source_id,
            qualifier: 0,
        },
        level: pin.level,
        fault_processing_disable: false,
    };
    let irte_back = crate::iommu::pin_irte(&route);
    let entry_ok = encoded.is_some() && entry_back == Some(rte);
    let irte_ok = irte_back == Some(expected);
    // What the I/O APIC and the unit hold must agree with each other: the same trigger mode,
    // and for a level-triggered pin the same vector.
    let agree = matches!((entry_back, irte_back), (Some(e), Some(i)) if e.agrees_with_irte(i.vector, i.level));
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: remapped: the PIT's IRQ0 (GSI {}, {trigger}-triggered, active-{polarity}) through IRTE {}: the redirection entry reads back in remappable format with that index, vector {:#x} and mask clear: {entry_ok}; the IRTE reads back vector {:#x} to {named}, {trigger}-triggered like the pin, checking source ID {:#06x} (I/O APIC {}'s, from the DMAR): {irte_ok}; the two agree: {agree}; {} earlier fault(s) cleared{}",
        pin.gsi,
        route.index,
        rte.vector,
        expected.vector,
        route.source_id,
        pin.id,
        stale.len(),
        if entry_ok && irte_ok && agree {
            ""
        } else {
            ": I/O APIC failed"
        }
    );

    // 2. Delivered: the PIT's interrupts arrive on the target, through the IRTE.
    let before = taken(&ids);
    let start = rdtsc();
    timer::pit_channel0_start(HZ);
    let from = ioapic_irqs_on(target);
    crate::sched::yield_until(
        || ioapic_irqs_on(target).wrapping_sub(from) >= PROOF_TICKS,
        tsc_hz,
        2,
    );
    let elapsed = rdtsc().wrapping_sub(start);
    let (arrived, elsewhere) = since(&ids, &before, target);
    // Nothing through this IRTE may have been blocked meanwhile.
    let (faults, lost) = crate::iommu::take_faults();
    let blocked = faults
        .iter()
        .find(|f| f.interrupt_index == Some(route.index))
        .copied();
    let delivered = arrived >= PROOF_TICKS && elsewhere == 0 && blocked.is_none();
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: remapped: {arrived} PIT interrupts arrived on {named} through IRTE {} in {} ms, {elsewhere} on other CPUs; fault recorded: {}{}; consistent {delivered}{}",
        route.index,
        elapsed.saturating_mul(1000) / tsc_hz.max(1),
        Recorded(blocked),
        if lost { " (a fault was lost)" } else { "" },
        if delivered { "" } else { ": I/O APIC failed" }
    );

    // 3. The IRTE not present: nothing may arrive, and the unit records why.
    let a = probe(&route, None, &ids, target, tsc_hz);
    let seen = a.fault.map(|f| f.source_id);
    let carries_id = seen == Some(route.source_id);
    let carries_none = seen == Some(NO_REQUESTER_ID) && route.source_id != NO_REQUESTER_ID;
    let a_ok = a.written
        && a.arrived == 0
        && a.fault
            .is_some_and(|f| reason(&f) == Some(REASON_NOT_PRESENT))
        && (carries_id || carries_none)
        && a.restored
        && a.resumed;
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: IRTE {} not present: {} interrupts arrived in {BLOCKED_MS} ms; fault recorded: {}{}{}; IRTE restored, {RESUME_TICKS} interrupts arrived on {named} again: {}; consistent {a_ok}{}",
        route.index,
        a.arrived,
        Recorded(a.fault),
        if a.lost { " (a fault was lost)" } else { "" },
        if carries_none {
            alloc::format!(
                ", from a message without a requester ID (the DMAR names {:#06x})",
                route.source_id
            )
        } else {
            alloc::string::String::new()
        },
        a.resumed,
        if a_ok { "" } else { ": I/O APIC failed" }
    );

    // 4. A wrong source ID: one function off the I/O APIC's.
    let wrong = route.source_id ^ 1;
    let b = probe(
        &route,
        Some(Irte {
            source: SourceCheck::RequesterId {
                source_id: wrong,
                qualifier: 0,
            },
            ..route.entry
        }),
        &ids,
        target,
        tsc_hz,
    );
    let enforced = b.arrived == 0
        && b.fault.is_some_and(|f| {
            reason(&f) == Some(REASON_SOURCE_ID) && f.source_id == route.source_id
        });
    let unchecked = b.arrived > 0 && b.fault.is_none();
    let b_ok = b.written
        && b.restored
        && b.resumed
        && if carries_id {
            enforced
        } else {
            carries_none && unchecked
        };
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: IRTE {} checking source ID {wrong:#06x} instead of {:#06x}: {} interrupts arrived in {BLOCKED_MS} ms; fault recorded: {}{}{}; IRTE restored, {RESUME_TICKS} interrupts arrived on {named} again: {}; consistent {b_ok}{}",
        route.index,
        route.source_id,
        b.arrived,
        Recorded(b.fault),
        if b.lost { " (a fault was lost)" } else { "" },
        if carries_none {
            "; the source-ID check cannot be shown on this platform: its I/O APIC's messages carry no requester ID (0xffff above), so the unit skips the check (hardware blocks them, reason 0x26)"
        } else {
            ""
        },
        b.resumed,
        if b_ok { "" } else { ": I/O APIC failed" }
    );

    // Masked, stopped, released: nothing uses the PIT after the boot proofs.
    if let Some((low, high)) = encoded {
        pin.io.set_redirection(pin.entry, low | MASKED, high);
    }
    timer::pit_channel0_stop();
    let masked = ioapic::is_masked(pin.io.redirection(pin.entry).0);
    let released = crate::iommu::release_pin(&route).is_ok();
    let (after, _) = crate::iommu::take_faults();
    let tidy = masked && released;
    let _ = writeln!(
        com1,
        "origin-kernel: I/O APIC: remapped: the entry masked, the PIT stopped and IRTE {} released: {tidy}; {} fault(s) recorded after{}",
        route.index,
        after.len(),
        if tidy { "" } else { ": I/O APIC failed" }
    );
    entry_ok && irte_ok && agree && delivered && a_ok && b_ok && tidy
}
