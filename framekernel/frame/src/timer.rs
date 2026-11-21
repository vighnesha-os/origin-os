//! The local APIC timer (M2: interrupts; CLAUDE.md §24, §68): x2APIC mode, calibration
//! against the 8254 PIT or, where it does not count, the ACPI PM timer or CPUID's TSC rate
//! (ADR-0013 F3), and a periodic interrupt proven by counting it.
//!
//! The register encodings and the calibration arithmetic are in `crate::apic`, tested
//! on the host. This module does the MSR and port I/O, in this order:
//! 1. mask both legacy 8259 PICs, so no firmware-programmed IRQ reaches a vector without a
//!    gate;
//! 2. switch the local APIC to x2APIC mode and software-enable it;
//! 3. calibrate: a 10 ms PIT channel-2 one-shot, during which the APIC timer counts down
//!    from 2^32 − 1 and the TSC counts up, repeated when a stall spoiled it
//!    ([`apic::CalibrationWindow`]);
//! 4. run the APIC timer periodically at [`HZ`], take [`PROOF_TICKS`] interrupts while
//!    polling with interrupt windows (never halting, so a lost interrupt cannot hang the
//!    check past its two-second deadline), and check the TSC-measured time ([`apic::judge_ticks`]);
//!    then read the timer's count against the TSC for its period ([`apic::judge_period`]). A
//!    run a stall made slow, or with no clean pair of count reads, is measured again, up to
//!    [`PROOF_ATTEMPTS`] runs ([`apic::tick_verdict`]);
//! 5. mask the timer again: nothing schedules on it yet (M3).

use core::fmt::Write;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::acpi;
use crate::apic::{
    self, CalibrationWindow, IA32_APIC_BASE, SPURIOUS_VECTOR, TIMER_VECTOR, TickVerdict, TimerMode,
    X2APIC_ID, X2APIC_LVT_TIMER, X2APIC_SVR, X2APIC_TIMER_CURRENT, X2APIC_TIMER_DIVIDE,
    X2APIC_TIMER_INITIAL, X2APIC_VERSION,
};
use crate::clock;

use crate::cpu::{self, rdmsr, rdtsc, wrmsr};
use crate::port::{inb, outb};

/// Periodic timer rate for the proof. 10 ms periods tolerate an emulator's wake-up latency
/// (QEMU's TCG coalesces timer expiries that a halted vCPU is late to take, which at 1 kHz
/// showed as 100 interrupts in 125 ms on the development host).
pub const HZ: u64 = 100;
/// Interrupts to take.
const PROOF_TICKS: u64 = 50;
/// Runs the periodic proof may make: another only after a run that said nothing about the rate
/// ([`apic::TickVerdict::Stalled`]), each against the same bounds. Each run is at most 2 s, so
/// a boot pays for more only while disturbed (TASKS item 66).
const PROOF_ATTEMPTS: u32 = 4;
/// Calibration window.
const CALIBRATION_MICROS: u64 = 10_000;
/// APIC timer divisor during calibration and afterwards.
const DIVISOR: u32 = 16;
/// How long, in TSC ticks, one calibration window may take before its reference is given up
/// on: about two seconds on any TSC, against a healthy window's 10 ms, so a counter that never
/// reaches its end does not hold up a boot for long.
const WINDOW_BUDGET_TICKS: u64 = 1 << 31;
/// Calibration windows to measure before taking the least disturbed one. A window is
/// spoiled by a stall near its start or its end, and each spoiled one costs 10 ms, so the
/// budget is spent only while windows are spoiled: on a real machine the first is clean.
/// Under QEMU's software processor the 65- and 67-processor machines spoil about 98 % of
/// them (TASKS item 66, 2026-11-19: in 7 of 14 idle boots none of 32 was clean, and the
/// least disturbed one was up to 2.9 % off, which the RTC second then refuted). With 1024,
/// a boot with no clean window is a chance of about 1 in 10^10 there; the budget is at
/// most about 10 s of windows.
const CALIBRATION_WINDOWS: u32 = 1024;

/// Master and slave 8259 data (mask) ports.
const PIC_MASTER_DATA: u16 = 0x21;
const PIC_SLAVE_DATA: u16 = 0xA1;

/// x2APIC registers this module reads. None has a read side effect.
#[derive(Clone, Copy, Debug)]
pub enum ApicRead {
    Id,
    Version,
    TimerCurrent,
    TimerInitial,
    TimerDivide,
    LvtTimer,
}

/// x2APIC registers this module writes: the timer and the spurious-vector register only,
/// so no write can send an interprocessor interrupt or touch memory.
#[derive(Clone, Copy)]
enum ApicWrite {
    SpuriousVector,
    LvtTimer,
    TimerInitial,
    TimerDivide,
}

/// The local APIC, in x2APIC mode: holding one proves the mode switch happened, so its
/// register MSRs exist.
#[derive(Debug)]
pub struct X2Apic(());

impl X2Apic {
    /// Switches the local APIC to x2APIC mode through the legal transitions and
    /// software-enables it with [`SPURIOUS_VECTOR`].
    ///
    /// # Safety
    ///
    /// CPL 0, boot processor only, interrupts disabled, and x2APIC supported
    /// ([`cpu::x2apic_supported`]); the spurious vector must have an IDT gate.
    pub unsafe fn enable() -> Self {
        // SAFETY: the caller's contract; IA32_APIC_BASE exists on every x86-64 processor.
        let current = unsafe { rdmsr(IA32_APIC_BASE) };
        let (writes, count) = apic::x2apic_transition(current);
        for &value in writes.iter().take(count) {
            // SAFETY: x2APIC is supported, and `x2apic_transition` yields only the legal
            // disabled → xAPIC → x2APIC sequence, keeping every other bit as it was.
            unsafe { wrmsr(IA32_APIC_BASE, value) };
        }
        let apic = Self(());
        apic.write(ApicWrite::SpuriousVector, apic::svr(SPURIOUS_VECTOR));
        if let Some(ready) = X2APIC_READY.get(crate::cpu::this_cpu().get()) {
            ready.store(true, core::sync::atomic::Ordering::Release);
        }
        apic
    }

    /// This processor's handle, if its local APIC has been switched to x2APIC mode
    /// ([`X2Apic::enable`] has run here): the safe way to get one, for the processor `me` names,
    /// read under a witness (ADR-0020 S3; TASKS item 93). Its uses stay in that witness's life.
    pub fn here(me: crate::cpu::ThisCpu<'_>) -> Option<Self> {
        X2APIC_READY
            .this(me)
            .is_some_and(|ready| ready.load(core::sync::atomic::Ordering::Acquire))
            .then_some(Self(()))
    }

    /// [`X2Apic::here`] without a witness: for the callers TASKS item 93 converts last (its
    /// shared files), and removed with them.
    pub fn this_cpu() -> Option<Self> {
        X2APIC_READY
            .get(crate::cpu::this_cpu().get())
            .is_some_and(|ready| ready.load(core::sync::atomic::Ordering::Acquire))
            .then_some(Self(()))
    }

    /// The handle for a processor whose local APIC is already in x2APIC mode, without
    /// touching it.
    ///
    /// # Safety
    ///
    /// CPL 0, and [`X2Apic::enable`] has run on this processor.
    pub unsafe fn assume_enabled() -> Self {
        Self(())
    }

    pub fn read(&self, reg: ApicRead) -> u64 {
        let msr = match reg {
            ApicRead::Id => X2APIC_ID,
            ApicRead::Version => X2APIC_VERSION,
            ApicRead::TimerCurrent => X2APIC_TIMER_CURRENT,
            ApicRead::TimerInitial => X2APIC_TIMER_INITIAL,
            ApicRead::TimerDivide => X2APIC_TIMER_DIVIDE,
            ApicRead::LvtTimer => X2APIC_LVT_TIMER,
        };
        // SAFETY: x2APIC mode is on (type invariant), so the register exists, and none of
        // these reads has a side effect.
        unsafe { rdmsr(msr) }
    }

    fn write(&self, reg: ApicWrite, value: u64) {
        let msr = match reg {
            ApicWrite::SpuriousVector => X2APIC_SVR,
            ApicWrite::LvtTimer => X2APIC_LVT_TIMER,
            ApicWrite::TimerInitial => X2APIC_TIMER_INITIAL,
            ApicWrite::TimerDivide => X2APIC_TIMER_DIVIDE,
        };
        // SAFETY: x2APIC mode is on (type invariant). These registers only configure the
        // local timer and the spurious vector; every vector they can name has an IDT gate
        // whose handler is correct for it.
        unsafe { wrmsr(msr, value) };
    }

    /// Sends an interprocessor interrupt: writes `command` (delivery mode, level, vector:
    /// ICR bits 0–19) with `destination` (an x2APIC ID) to the ICR.
    ///
    /// # Safety
    ///
    /// The IPI's effect on the destination must be one the caller intends and has prepared
    /// for; an INIT or start-up IPI, for example, needs a ready trampoline.
    pub unsafe fn send_ipi(&self, destination: u32, command: u32) {
        let value = (u64::from(destination) << 32) | u64::from(command & 0xF_FFFF);
        // SAFETY: x2APIC mode is on (type invariant), so the ICR exists; the caller vouches
        // for the IPI's effect.
        unsafe { wrmsr(X2APIC_ICR, value) };
    }

    /// Stops the timer: masked, and a zero initial count.
    fn stop_timer(&self) {
        self.write(
            ApicWrite::LvtTimer,
            apic::lvt_timer(TIMER_VECTOR, TimerMode::OneShot, true),
        );
        self.write(ApicWrite::TimerInitial, 0);
    }
}

/// ICR level bit: assert (the only meaningful level for fixed IPIs).
const ICR_ASSERT: u32 = 1 << 14;

/// Each CPU index whose local APIC [`X2Apic::enable`] switched to x2APIC mode.
pub(crate) static X2APIC_READY: crate::percpu::PerCpu<core::sync::atomic::AtomicBool> =
    crate::percpu::PerCpu::new(
        "origin_frame::timer::X2APIC_READY",
        core::sync::atomic::AtomicBool::new(false),
        || core::sync::atomic::AtomicBool::new(false),
    );

/// Sends the reschedule IPI to the processor with APIC ID `apic_id`: its handler acknowledges
/// and switches through that processor's own run queue. Nothing happens if this processor's
/// APIC is not in x2APIC mode yet. Safe: an IPI with this fixed vector does nothing a timer
/// interrupt could not do, and the destination cannot be harmed by a stray one (its handler
/// only reschedules).
pub fn send_reschedule_ipi(me: crate::cpu::ThisCpu<'_>, apic_id: u32) {
    if let Some(lapic) = X2Apic::here(me) {
        let value = (u64::from(apic_id) << 32)
            | u64::from(ICR_ASSERT | u32::from(crate::cpu::RESCHEDULE_VECTOR));
        // SAFETY: x2APIC mode is on (`here`), so the ICR exists; a fixed IPI with the
        // reschedule vector only reschedules its destination.
        unsafe { crate::cpu::wrmsr(X2APIC_ICR, value) };
        let _ = lapic;
    }
}

/// The ICR's low half for an NMI, to a destination and without a shorthand: Intel's table of
/// valid ICR combinations marks the Self shorthand with NMI delivery invalid (ADR-0013 F1).
const ICR_NMI: u64 = 0x4400;

/// Sends this processor an NMI (x2APIC ICR, NMI delivery, addressed to its own APIC ID).
/// Nothing happens if its APIC is not in x2APIC mode. Safe: the NMI handler only records it.
pub fn send_nmi_to_self(me: crate::cpu::ThisCpu<'_>) {
    if let Some(lapic) = X2Apic::here(me) {
        let own = lapic.read(ApicRead::Id);
        // SAFETY: x2APIC mode is on (`here`), so the ICR exists; an NMI to this
        // processor runs the kernel's NMI handler, which only counts it.
        unsafe { crate::cpu::wrmsr(X2APIC_ICR, (own << 32) | ICR_NMI) };
    }
}

/// Makes the I/O APIC's interrupt ([`crate::cpu::IOAPIC_VECTOR`]) pending on this processor,
/// as if its device had raised it (x2APIC ICR, fixed delivery, "self" shorthand), so that a
/// proof can see where it is taken: with interrupts off, it waits until something lets it in.
/// Nothing happens if this processor's APIC is not in x2APIC mode. Safe: the vector has a gate
/// and its handler is safe code, so this is no more than a device can do at any moment.
pub fn interrupt_self(me: crate::cpu::ThisCpu<'_>) {
    if let Some(lapic) = X2Apic::here(me) {
        // SAFETY: fixed delivery of a vector with an IDT gate, to this processor alone: the
        // same as the device behind that vector interrupting it.
        unsafe {
            lapic.send_ipi(
                0,
                (1 << 18) | (1 << 14) | u32::from(crate::cpu::IOAPIC_VECTOR),
            )
        };
    }
}

/// The legacy PC ports this module drives: the 8259 mask registers, PIT channel 2 and
/// port 0x61.
struct LegacyPorts(());

impl LegacyPorts {
    /// # Safety
    ///
    /// CPL 0, boot processor only, and nothing else drives the 8259s, PIT channel 2 or
    /// port 0x61. None of these registers can affect memory.
    unsafe fn new() -> Self {
        Self(())
    }

    fn write(&mut self, port: u16, value: u8) {
        // SAFETY: the constructor's contract; `port` is one of the registers named above.
        unsafe { outb(port, value) };
    }

    fn read(&mut self, port: u16) -> u8 {
        // SAFETY: as for `write`.
        unsafe { inb(port) }
    }

    /// Masks every 8259 line, so no firmware-programmed legacy IRQ is delivered.
    fn mask_pics(&mut self) {
        self.write(PIC_MASTER_DATA, 0xFF);
        self.write(PIC_SLAVE_DATA, 0xFF);
    }
}

/// x2APIC interrupt command register.
const X2APIC_ICR: u32 = 0x830;

/// PIT channel 0's data port (the PIT is reserved to this module: `port::PortRange`).
const PIT_CHANNEL0: u16 = 0x40;
/// Channel 0, access lo/hi byte, mode 2 (rate generator), binary.
const PIT_CHANNEL0_MODE2: u8 = 0x34;
/// Channel 0, access lo/hi byte, mode 0 (one-shot): used to stop the rate generator.
const PIT_CHANNEL0_MODE0: u8 = 0x30;

/// Programs the PIT's channel 0 as a rate generator at `hz` (the interrupt-routing proofs).
/// Only the boot processor uses channel 0, and only after [`init`] (channel 2 and
/// calibration are done by then).
pub fn pit_channel0_start(hz: u64) {
    let divisor = (apic::PIT_HZ / hz.max(1)) as u16;
    // SAFETY: CPL 0; the PIT is reserved to this module (`port::PortRange` refuses its ports),
    // and it cannot affect memory.
    unsafe { outb(apic::PIT_COMMAND, PIT_CHANNEL0_MODE2) };
    // SAFETY: as above; the low byte of the divisor.
    unsafe { outb(PIT_CHANNEL0, (divisor & 0xFF) as u8) };
    // SAFETY: as above; the high byte.
    unsafe { outb(PIT_CHANNEL0, (divisor >> 8) as u8) };
}

/// Whether channel 0 has been counting: two latched readings a millisecond apart differ. A
/// gated 8254 takes its programming and never counts, and with no 8254 at all the ports read
/// the same. [`pit_channel0_start`] must have run.
pub fn pit_channel0_counting(tsc_hz: u64) -> bool {
    let read = || {
        // SAFETY: as for `pit_channel0_start`; the latch command.
        unsafe { outb(apic::PIT_COMMAND, 0) };
        // SAFETY: as above; the latched count's low byte.
        let low = unsafe { inb(PIT_CHANNEL0) };
        // SAFETY: as above; the high byte.
        let high = unsafe { inb(PIT_CHANNEL0) };
        u16::from_le_bytes([low, high])
    };
    let first = read();
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < tsc_hz / 1000 {
        core::hint::spin_loop();
    }
    read() != first
}

/// Stops the PIT's channel 0 (mode 0, count 0: it fires once and then idles).
pub fn pit_channel0_stop() {
    // SAFETY: as for `pit_channel0_start`; the command port.
    unsafe { outb(apic::PIT_COMMAND, PIT_CHANNEL0_MODE0) };
    // SAFETY: as above; a zero count, low then high byte.
    unsafe { outb(PIT_CHANNEL0, 0) };
    // SAFETY: as above.
    unsafe { outb(PIT_CHANNEL0, 0) };
}

/// The TSC rate [`init`] measured, in Hz (0 before it has): the framework's own, for the AP
/// start-up delays, so that nothing above it can shorten them.
static CALIBRATED_TSC_HZ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// The TSC rate [`init`] measured, once it has.
pub fn calibrated_tsc_hz() -> Option<u64> {
    match CALIBRATED_TSC_HZ.load(core::sync::atomic::Ordering::Acquire) {
        0 => None,
        hz => Some(hz),
    }
}

/// The calibrated initial count for [`HZ`] interrupts per second, once known (0 before).
static PERIOD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The boot processor's timer as [`init`]'s timed proof ran it: its initial count, divide
/// configuration and LVT entry, read from its registers in the run that passed, before the timer
/// was stopped. What every processor's timer must read back as in the preemption proof (TASKS
/// item 66, the addendum review's T66-R14: read from the timed run, not rebuilt from
/// [`PERIOD`]). Zero until the proof passes.
static TIMED: [core::sync::atomic::AtomicU64; 3] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 3];

/// The boot processor's timer as its timed proof ran it ([`TIMED`]); `None` before the proof
/// passed.
pub fn timed_programming() -> Option<apic::TimerProgramming> {
    let [initial, divide, lvt] =
        core::array::from_fn(|i| TIMED[i].load(core::sync::atomic::Ordering::Acquire));
    (initial != 0).then(|| apic::TimerProgramming::from_registers(initial, divide, lvt))
}

/// This processor's timer as programmed, read back from its x2APIC through [`X2Apic::read`];
/// `None` if x2APIC mode is not on. For the preemption proof's check that every processor
/// ticks as the boot processor's proof timed (TASKS item 66, the review's T66-R1).
pub fn programmed(here: crate::trap::Here<'_>) -> Option<apic::TimerProgramming> {
    let lapic = X2Apic::here(crate::cpu::this_cpu_at(here))?;
    Some(apic::TimerProgramming::from_registers(
        lapic.read(ApicRead::TimerInitial),
        lapic.read(ApicRead::TimerDivide),
        lapic.read(ApicRead::LvtTimer),
    ))
}

/// Starts this processor's APIC timer at the calibrated [`HZ`]. Returns false if the
/// boot processor has not calibrated it.
pub fn start_periodic(lapic: &X2Apic) -> bool {
    let count = PERIOD.load(core::sync::atomic::Ordering::Acquire);
    if count == 0 {
        return false;
    }
    lapic.write(
        ApicWrite::TimerDivide,
        apic::divide_config(DIVISOR).unwrap_or(0b0011),
    );
    lapic.write(
        ApicWrite::LvtTimer,
        apic::lvt_timer(TIMER_VECTOR, TimerMode::Periodic, false),
    );
    lapic.write(ApicWrite::TimerInitial, u64::from(count));
    true
}

impl X2Apic {
    /// Stops this processor's APIC timer.
    pub fn stop(&self) {
        self.stop_timer();
    }
}

/// What the calibration can run against, in the order it tries them (ADR-0013 F3).
const SOURCES: [&str; 3] = ["the PIT", "the ACPI PM timer", "CPUID 15h"];
/// The index into [`SOURCES`] of what the calibration ran against.
static CALIBRATED_AGAINST: AtomicU8 = AtomicU8::new(0);

/// What the APIC timer and the TSC were calibrated against, for the clock's report.
pub fn calibration_source() -> &'static str {
    SOURCES
        .get(usize::from(CALIBRATED_AGAINST.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or("the PIT")
}

/// The TSC rate CPUID leaf 15h enumerates, if it does.
pub fn cpuid_tsc_hz() -> Option<u64> {
    if cpu::cpuid(0)[0] < 0x15 {
        return None;
    }
    let [eax, ebx, ecx, _] = cpu::cpuid(0x15);
    clock::cpuid_tsc_hz(eax, ebx, ecx)
}

/// A counter that runs at a known rate, to measure the APIC timer and the TSC against when
/// the 8254 cannot be used.
#[derive(Clone, Copy)]
enum Reference {
    /// The ACPI PM timer: a free-running 3.579545 MHz counter, 24 or 32 bits wide.
    PmTimer { port: u16, mask: u64 },
    /// The TSC itself, at the rate CPUID leaf 15h enumerates.
    CpuidTsc { hz: u64 },
}

impl Reference {
    fn source(self) -> usize {
        match self {
            Self::PmTimer { .. } => 1,
            Self::CpuidTsc { .. } => 2,
        }
    }

    fn hz(self) -> u64 {
        match self {
            Self::PmTimer { .. } => acpi::PM_TIMER_HZ,
            Self::CpuidTsc { hz } => hz,
        }
    }

    fn mask(self) -> u64 {
        match self {
            Self::PmTimer { mask, .. } => mask,
            Self::CpuidTsc { .. } => u64::MAX,
        }
    }

    fn read(self) -> u64 {
        match self {
            // SAFETY: CPL 0, boot processor; `port` is the PM timer's port as the FADT names it
            // (`X_PM_TMR_BLK` or `PM_TMR_BLK`, chosen by `acpi::parse_fadt`), a read-only timer
            // that cannot affect memory, and `pm_timer` refused a system-owned port.
            Self::PmTimer { port, mask } => u64::from(unsafe { crate::port::ind(port) }) & mask,
            Self::CpuidTsc { .. } => rdtsc(),
        }
    }
}

/// The ACPI PM timer as a reference, if the firmware's FADT names one.
fn pm_timer(rsdp: u64) -> Option<Reference> {
    let fadt = crate::firmware::acpi_table(rsdp, b"FACP").ok()?;
    let fadt = acpi::parse_fadt(fadt).ok()?;
    let mask = if fadt.pm_timer_32bit {
        0xFFFF_FFFF
    } else {
        0x00FF_FFFF
    };
    let port = fadt.pm_timer_port?;
    // The proof in `time.rs` claims this port, which can happen only once, so this reads it
    // without a claim; a claim's refusal of the system's ports is kept here.
    if crate::port::reserved(port, 4) {
        return None;
    }
    Some(Reference::PmTimer { port, mask })
}

/// Brings up the local APIC and proves its timer, with the ACPI tables at `rsdp` for a
/// reference other than the PIT if it does not count. Returns the TSC rate in Hz if every
/// step passed.
pub fn init(com1: &mut dyn Write, rsdp: u64) -> Option<u64> {
    // Every vector this can make arrive needs the kernel's own IDT.
    if !cpu::tables_ready() {
        let _ = writeln!(
            com1,
            "origin-kernel: the APIC cannot start before the CPU's tables"
        );
        return None;
    }
    if !cpu::x2apic_supported() {
        let _ = writeln!(com1, "origin-kernel: the processor has no x2APIC");
        return None;
    }
    // SAFETY: CPL 0, boot processor only (entry contract); no other code uses these ports.
    let mut ports = unsafe { LegacyPorts::new() };
    ports.mask_pics();
    // SAFETY: CPL 0, boot processor, interrupts disabled, x2APIC supported (checked), and
    // `cpu::init` installed a gate for the spurious vector.
    let lapic = unsafe { X2Apic::enable() };
    let version = lapic.read(ApicRead::Version);
    let _ = writeln!(
        com1,
        "origin-kernel: x2APIC enabled: APIC ID {}, version {:#x}, {} LVT entries",
        lapic.read(ApicRead::Id),
        version & 0xFF,
        ((version >> 16) & 0xFF) + 1
    );

    let Some(((apic_hz, tsc_hz, disturbed), source)) = calibrate(&lapic, &mut ports, rsdp) else {
        let _ = writeln!(com1, "origin-kernel: APIC timer calibration failed");
        return None;
    };
    CALIBRATED_AGAINST.store(source as u8, Ordering::Relaxed);
    let source = calibration_source();
    let _ = write!(
        com1,
        "origin-kernel: APIC timer calibrated against {source}: {} kHz (divide by {DIVISOR}), TSC {} MHz",
        apic_hz / 1000,
        tsc_hz / 1_000_000
    );
    if disturbed != 0 {
        let noun = if disturbed == 1 { "window" } else { "windows" };
        let _ = write!(com1, " ({disturbed} disturbed {noun} discarded)");
    }
    let _ = writeln!(com1);
    let Some(count) = apic::initial_count(apic_hz, HZ) else {
        let _ = writeln!(
            com1,
            "origin-kernel: APIC timer rate {apic_hz} Hz is unusable"
        );
        return None;
    };

    // The rate from the timer's count against the TSC: its period within 97 % to 103 % of
    // nominal, whatever the interrupts did (the addendum review's T66-R13). The interrupts'
    // delivery from their stamps: all 50 within the 2 s limit, not under 95 % of their nominal
    // time in total, and at most 50 % slower (an emulator may coalesce late expiries; physical
    // hardware qualification will tighten that, CLAUDE.md §74). A run with no clean pair of
    // count reads, or whose delivery a stall made slow, and only such a run, is measured again
    // against the same bounds (`apic::tick_verdict`).
    let period = tsc_hz / HZ;
    let micros = |ticks: u64| ticks.saturating_mul(1_000_000) / tsc_hz.max(1);
    let expected = PROOF_TICKS * 1_000_000 / HZ;
    let mut stamps = [0u64; PROOF_TICKS as usize];
    let mut attempts = 0;
    let (run, verdict, seen_period, timed, lasted) = loop {
        attempts += 1;
        let timed = take_ticks(&lapic, count, tsc_hz, &mut stamps);
        let seen = stamps.get(..timed.taken).unwrap_or(&[]);
        let run = apic::judge_ticks(timed.start, seen, PROOF_TICKS as usize, period);
        let verdict = apic::tick_verdict(run.verdict, apic::judge_period(timed.sample, period));
        let seen_period = PeriodSeen {
            ppm: timed.sample.map(|sample| sample.ppm_of(period)),
            tries: timed.tries,
        };
        if verdict != TickVerdict::Stalled || attempts == PROOF_ATTEMPTS {
            // A short run lasted until its limit, not until its last interrupt.
            let lasted = if (timed.taken as u64) < PROOF_TICKS {
                timed.end.wrapping_sub(timed.start)
            } else {
                run.elapsed
            };
            break (run, verdict, seen_period, timed, lasted);
        }
        // Each run measured again is said, with what made it say nothing: a repeat is a finding
        // on a machine without a hypervisor (ADR-0013's note of TASKS item 66). Lines stay under
        // the console's 160 bytes (the review's T66-R7).
        let _ = writeln!(
            com1,
            "origin-kernel: APIC timer: run {attempts} of {PROOF_ATTEMPTS} disturbed ({} ticks in {} ms, longest gap {} ms, {seen_period}), measured again",
            timed.taken,
            micros(run.elapsed) / 1000,
            micros(run.longest_gap) / 1000
        );
    };
    let taken = timed.taken;
    if verdict != TickVerdict::InBounds {
        let _ = writeln!(
            com1,
            "origin-kernel: {taken} ticks in {} us, not {PROOF_TICKS} in {expected} (median gap {} us, longest {} us, {}, run {attempts} of {PROOF_ATTEMPTS}): APIC timer is wrong",
            micros(lasted),
            micros(run.median_gap),
            micros(run.longest_gap),
            seen_period.short()
        );
        return None;
    }
    let _ = write!(
        com1,
        "origin-kernel: APIC timer: {PROOF_TICKS} interrupts at {HZ} Hz in {} ms (initial count {count}, {seen_period})",
        micros(run.elapsed) / 1000
    );
    if attempts > 1 {
        let _ = write!(com1, ", run {attempts} of {PROOF_ATTEMPTS}");
    }
    let _ = writeln!(com1);
    let registers = timed.registers;
    for (cell, value) in TIMED.iter().zip(registers) {
        cell.store(value, core::sync::atomic::Ordering::Release);
    }
    PERIOD.store(count, core::sync::atomic::Ordering::Release);
    // The first measurement stands.
    let _ = CALIBRATED_TSC_HZ.compare_exchange(
        0,
        tsc_hz,
        core::sync::atomic::Ordering::AcqRel,
        core::sync::atomic::Ordering::Acquire,
    );
    Some(tsc_hz)
}

/// Runs the APIC timer periodically with `count` and stamps each interrupt with the TSC at the
/// poll that saw it, until `stamps` is full or two seconds pass, polling with interrupt windows
/// (never halting, so a lost interrupt cannot hang the check). Returns the TSC at the start and
/// how many interrupts were stamped, and the TSC at the end. The timer is stopped again.
fn take_ticks(lapic: &X2Apic, count: u32, tsc_hz: u64, stamps: &mut [u64]) -> TimedRun {
    // An expiry a stopped run left pending is taken here, before the count starts, so that a
    // repeated run cannot begin one interrupt ahead (the review's T66-R6).
    cpu::interrupt_window();
    let before = cpu::ticks();
    let start = rdtsc();
    let limit = tsc_hz.saturating_mul(2); // two seconds
    lapic.write(
        ApicWrite::LvtTimer,
        apic::lvt_timer(TIMER_VECTOR, TimerMode::Periodic, false),
    );
    lapic.write(ApicWrite::TimerInitial, u64::from(count));
    let mut seen = 0;
    while seen < stamps.len() && rdtsc().wrapping_sub(start) < limit {
        cpu::interrupt_window();
        let taken = cpu::ticks().wrapping_sub(before);
        let now = rdtsc();
        while (seen as u64) < taken {
            let Some(slot) = stamps.get_mut(seen) else {
                break;
            };
            *slot = now;
            seen += 1;
        }
        core::hint::spin_loop();
    }
    let end = rdtsc();
    // The registers as this run ran them, read before the timer stops: the programming every
    // processor's timer must read back as (`TIMED`, the addendum review's T66-R14).
    let registers = [
        lapic.read(ApicRead::TimerInitial),
        lapic.read(ApicRead::TimerDivide),
        lapic.read(ApicRead::LvtTimer),
    ];
    // The timer's period from its count, still running as timed (the addendum review's
    // T66-R13).
    let (sample, tries) = sample_period(lapic, registers[0], tsc_hz / HZ);
    lapic.stop_timer();
    TimedRun {
        start,
        taken: seen,
        end,
        registers,
        sample,
        tries,
    }
}

/// One run of the periodic proof, as [`take_ticks`] took it.
struct TimedRun {
    /// The TSC at the timer's start and at the run's end, and the interrupts stamped.
    start: u64,
    taken: usize,
    end: u64,
    /// The timer's initial count, divide configuration and LVT entry as the run held them.
    registers: [u64; 3],
    /// The first clean pair of reads of its count, if one came, and the pairs tried.
    sample: Option<apic::PeriodSample>,
    tries: u32,
}

/// How a run's count showed its period, for the proof's lines (the coordinating session's
/// ruling on the addendum review's T66-R13): in parts per million of nominal, with the pairs
/// of reads it took.
struct PeriodSeen {
    ppm: Option<u64>,
    tries: u32,
}

impl PeriodSeen {
    /// Without the pairs, for the failure line.
    fn short(&self) -> PeriodShort {
        PeriodShort(self.ppm)
    }
}

impl core::fmt::Display for PeriodSeen {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.ppm {
            Some(ppm) => write!(f, "period {ppm} ppm from {} pairs", self.tries),
            None => write!(f, "period undecided after {} pairs", self.tries),
        }
    }
}

struct PeriodShort(Option<u64>);

impl core::fmt::Display for PeriodShort {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(ppm) => write!(f, "period {ppm} ppm"),
            None => write!(f, "period undecided"),
        }
    }
}

/// One read of the timer's current count, bracketed by the TSC.
fn read_count(lapic: &X2Apic) -> apic::CountRead {
    let before = rdtsc();
    let count = lapic.read(ApicRead::TimerCurrent);
    let after = rdtsc();
    apic::CountRead {
        before,
        count,
        after,
    }
}

/// Measures the running periodic timer's period from its count, reloaded with `initial`
/// ([`apic::sample_period`], which judges the reads).
fn sample_period(lapic: &X2Apic, initial: u64, nominal: u64) -> (Option<apic::PeriodSample>, u32) {
    apic::sample_period(initial, nominal, || read_count(lapic))
}

/// One PIT channel-2 one-shot of `pit` input clocks, with the APIC timer counting down and
/// the TSC counting up: the APIC ticks that elapsed, the TSC readings around the window, and
/// whether OUT2 was seen low before it rose. OUT2 high at the first poll means this window
/// says nothing about the PIT's rate: a stall, or a bus with nothing on it that reads 0xFF.
/// `None` if the PIT never reached terminal count.
fn measure_window(
    lapic: &X2Apic,
    ports: &mut LegacyPorts,
    pit: u16,
) -> Option<(u64, CalibrationWindow, bool)> {
    lapic.stop_timer();
    // Gate low and speaker off while programming channel 2 for mode 0.
    let port61 = ports.read(apic::PIT_GATE_PORT) & !0b11;
    ports.write(apic::PIT_GATE_PORT, port61);
    ports.write(apic::PIT_COMMAND, apic::PIT_CHANNEL2_MODE0);
    ports.write(apic::PIT_CHANNEL2, (pit & 0xFF) as u8);
    // Start the three clocks together: the PIT's count (a real 8254 counts from the gate's
    // rise, QEMU's model from the count's last byte, so the bracket covers both), the APIC
    // count-down and the TSC.
    let before_start = rdtsc();
    ports.write(apic::PIT_CHANNEL2, (pit >> 8) as u8);
    ports.write(apic::PIT_GATE_PORT, port61 | 1);
    lapic.write(ApicWrite::TimerInitial, u64::from(u32::MAX));
    let after_start = rdtsc();
    let mut before_end = after_start;
    let mut seen_low = false;
    // OUT2 (port 0x61 bit 5) rises at terminal count.
    loop {
        let before = rdtsc();
        if ports.read(apic::PIT_GATE_PORT) & 0x20 != 0 {
            break;
        }
        before_end = before;
        seen_low = true;
        if before.wrapping_sub(after_start) >= WINDOW_BUDGET_TICKS {
            lapic.stop_timer();
            return None;
        }
    }
    let after_end = rdtsc();
    let current = lapic.read(ApicRead::TimerCurrent);
    let after_read = rdtsc();
    lapic.stop_timer();
    ports.write(apic::PIT_GATE_PORT, port61);
    let apic_ticks = u64::from(u32::MAX).checked_sub(current)?;
    Some((
        apic_ticks,
        CalibrationWindow {
            before_start,
            after_start,
            before_end,
            after_end,
            after_read,
        },
        seen_low,
    ))
}

/// One window of [`CALIBRATION_MICROS`] measured against `reference`: the APIC ticks that
/// elapsed, the TSC readings around the window and how many reference ticks it really took.
/// `None` if the reference did not reach its target within [`WINDOW_BUDGET_TICKS`].
fn measure_against(
    lapic: &X2Apic,
    reference: Reference,
    target: u64,
) -> Option<(u64, CalibrationWindow, u64)> {
    lapic.stop_timer();
    let before_start = rdtsc();
    let first = reference.read();
    lapic.write(ApicWrite::TimerInitial, u64::from(u32::MAX));
    let after_start = rdtsc();
    let mut before_end = after_start;
    let last = loop {
        let before = rdtsc();
        let value = reference.read();
        if apic::counter_elapsed(first, value, reference.mask()) >= target {
            break value;
        }
        before_end = before;
        if before.wrapping_sub(after_start) >= WINDOW_BUDGET_TICKS {
            lapic.stop_timer();
            return None;
        }
    };
    let after_end = rdtsc();
    let current = lapic.read(ApicRead::TimerCurrent);
    let after_read = rdtsc();
    lapic.stop_timer();
    let apic_ticks = u64::from(u32::MAX).checked_sub(current)?;
    Some((
        apic_ticks,
        CalibrationWindow {
            before_start,
            after_start,
            before_end,
            after_end,
            after_read,
        },
        apic::counter_elapsed(first, last, reference.mask()),
    ))
}

/// Measures the APIC timer and the TSC against `reference`, by the same rules as
/// [`calibrate_pit`]. The windows are as long as the reference says they really were.
fn calibrate_against(lapic: &X2Apic, reference: Reference) -> Option<(u64, u64, u32)> {
    let target = apic::window_ticks(reference.hz(), CALIBRATION_MICROS)?;
    let mut best: Option<(u64, CalibrationWindow, u64)> = None;
    let mut disturbed = 0;
    for _ in 0..CALIBRATION_WINDOWS {
        let (apic_ticks, window, ticks) = measure_against(lapic, reference, target)?;
        if window.clean() {
            best = Some((apic_ticks, window, ticks));
            break;
        }
        disturbed += 1;
        if best.is_none_or(|(_, kept, _)| window.skew() < kept.skew()) {
            best = Some((apic_ticks, window, ticks));
        }
    }
    let (apic_ticks, window, ticks) = best?;
    let nanos = apic::ticks_nanos(ticks, reference.hz())?;
    Some((
        apic::rate(apic_ticks, nanos)?,
        apic::rate(window.ticks(), nanos)?,
        disturbed,
    ))
}

/// Measures the APIC timer and the TSC against the first source that works: the PIT, then
/// the ACPI PM timer, then CPUID's TSC rate. Returns their rates in Hz, the windows discarded
/// and the index of the source ([`SOURCES`]), or `None` if none worked.
fn calibrate(
    lapic: &X2Apic,
    ports: &mut LegacyPorts,
    rsdp: u64,
) -> Option<((u64, u64, u32), usize)> {
    lapic.write(ApicWrite::TimerDivide, apic::divide_config(DIVISOR)?);
    if let Some(found) = calibrate_pit(lapic, ports) {
        return Some((found, 0));
    }
    [
        pm_timer(rsdp),
        cpuid_tsc_hz().map(|hz| Reference::CpuidTsc { hz }),
    ]
    .into_iter()
    .flatten()
    .find_map(|reference| {
        calibrate_against(lapic, reference).map(|found| (found, reference.source()))
    })
}

/// Measures the APIC timer and the TSC against the PIT, repeating a window that a stall
/// spoiled (see [`CalibrationWindow`]) and settling, after [`CALIBRATION_WINDOWS`], for the
/// least disturbed one (the RTC proof then decides whether it is good enough). Returns
/// their rates in Hz and the windows it discarded, or `None` if the PIT did not count: no
/// terminal count, or OUT2 high from the first poll of every window.
fn calibrate_pit(lapic: &X2Apic, ports: &mut LegacyPorts) -> Option<(u64, u64, u32)> {
    let pit = apic::pit_count(CALIBRATION_MICROS)?;
    let nanos = apic::pit_nanos(pit);
    let mut best: Option<(u64, CalibrationWindow)> = None;
    let mut disturbed = 0;
    for _ in 0..CALIBRATION_WINDOWS {
        let (apic_ticks, window, seen_low) = measure_window(lapic, ports, pit)?;
        if !seen_low {
            continue;
        }
        if window.clean() {
            best = Some((apic_ticks, window));
            break;
        }
        disturbed += 1;
        if best.is_none_or(|(_, kept)| window.skew() < kept.skew()) {
            best = Some((apic_ticks, window));
        }
    }
    let (apic_ticks, window) = best?;
    Some((
        apic::rate(apic_ticks, nanos)?,
        apic::rate(window.ticks(), nanos)?,
        disturbed,
    ))
}
