//! The local APIC timer (M2: interrupts; CLAUDE.md §24, §68): x2APIC mode, calibration
//! against the 8254 PIT, and a periodic interrupt proven by counting it.
//!
//! The register encodings and the calibration arithmetic are in `framekernel::apic`, tested
//! on the host. This module does the MSR and port I/O, in this order:
//! 1. mask both legacy 8259 PICs, so no firmware-programmed IRQ reaches a vector without a
//!    gate;
//! 2. switch the local APIC to x2APIC mode and software-enable it;
//! 3. calibrate: a 10 ms PIT channel-2 one-shot, during which the APIC timer counts down
//!    from 2^32 − 1 and the TSC counts up;
//! 4. run the APIC timer periodically at [`HZ`], take [`PROOF_TICKS`] interrupts while
//!    polling with interrupt windows (never halting, so a lost interrupt cannot hang the
//!    check past its two-second deadline), and check the TSC-measured time;
//! 5. mask the timer again: nothing schedules on it yet (M3).

use core::fmt::Write;

use framekernel::apic::{
    self, IA32_APIC_BASE, SPURIOUS_VECTOR, TIMER_VECTOR, TimerMode, X2APIC_ID, X2APIC_LVT_TIMER,
    X2APIC_SVR, X2APIC_TIMER_CURRENT, X2APIC_TIMER_DIVIDE, X2APIC_TIMER_INITIAL, X2APIC_VERSION,
};

use crate::console::Console;
use crate::cpu::{self, rdmsr, rdtsc, wrmsr};
use crate::{inb, outb};

/// Periodic timer rate for the proof. 10 ms periods tolerate an emulator's wake-up latency
/// (QEMU's TCG coalesces timer expiries that a halted vCPU is late to take, which at 1 kHz
/// showed as 100 interrupts in 125 ms on the development host).
pub(crate) const HZ: u64 = 100;
/// Interrupts to take.
const PROOF_TICKS: u64 = 50;
/// Calibration window.
const CALIBRATION_MICROS: u64 = 10_000;
/// APIC timer divisor during calibration and afterwards.
const DIVISOR: u32 = 16;
/// Port reads to wait for the PIT before giving up.
const PIT_POLL_LIMIT: u32 = 50_000_000;

/// Master and slave 8259 data (mask) ports.
const PIC_MASTER_DATA: u16 = 0x21;
const PIC_SLAVE_DATA: u16 = 0xA1;

/// x2APIC registers this module reads. None has a read side effect.
#[derive(Clone, Copy)]
pub(crate) enum ApicRead {
    Id,
    Version,
    TimerCurrent,
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
pub(crate) struct X2Apic(());

impl X2Apic {
    /// Switches the local APIC to x2APIC mode through the legal transitions and
    /// software-enables it with [`SPURIOUS_VECTOR`].
    ///
    /// # Safety
    ///
    /// CPL 0, boot processor only, interrupts disabled, and x2APIC supported
    /// ([`cpu::x2apic_supported`]); the spurious vector must have an IDT gate.
    pub(crate) unsafe fn enable() -> Self {
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
        apic
    }

    /// The handle for a processor whose local APIC is already in x2APIC mode, without
    /// touching it.
    ///
    /// # Safety
    ///
    /// CPL 0, and [`X2Apic::enable`] has run on this processor.
    pub(crate) unsafe fn assume_enabled() -> Self {
        Self(())
    }

    pub(crate) fn read(&self, reg: ApicRead) -> u64 {
        let msr = match reg {
            ApicRead::Id => X2APIC_ID,
            ApicRead::Version => X2APIC_VERSION,
            ApicRead::TimerCurrent => X2APIC_TIMER_CURRENT,
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
    pub(crate) unsafe fn send_ipi(&self, destination: u32, command: u32) {
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

/// The calibrated initial count for [`HZ`] interrupts per second, once known (0 before).
static PERIOD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Starts this processor's APIC timer at the calibrated [`HZ`]. Returns false if the
/// boot processor has not calibrated it.
pub(crate) fn start_periodic(lapic: &X2Apic) -> bool {
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
    pub(crate) fn stop(&self) {
        self.stop_timer();
    }
}

/// Brings up the local APIC and proves its timer. Returns the TSC rate in Hz if every step
/// passed.
pub(crate) fn init(com1: &mut Console) -> Option<u64> {
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

    let Some((apic_hz, tsc_hz)) = calibrate(&lapic, &mut ports) else {
        let _ = writeln!(com1, "origin-kernel: APIC timer calibration failed");
        return None;
    };
    let _ = writeln!(
        com1,
        "origin-kernel: APIC timer calibrated against the PIT: {} kHz (divide by {DIVISOR}), TSC {} MHz",
        apic_hz / 1000,
        tsc_hz / 1_000_000
    );
    let Some(count) = apic::initial_count(apic_hz, HZ) else {
        let _ = writeln!(
            com1,
            "origin-kernel: APIC timer rate {apic_hz} Hz is unusable"
        );
        return None;
    };

    let before = cpu::ticks();
    let start = rdtsc();
    let limit = tsc_hz.saturating_mul(2); // two seconds
    lapic.write(
        ApicWrite::LvtTimer,
        apic::lvt_timer(TIMER_VECTOR, TimerMode::Periodic, false),
    );
    lapic.write(ApicWrite::TimerInitial, u64::from(count));
    while cpu::ticks() - before < PROOF_TICKS && rdtsc().wrapping_sub(start) < limit {
        cpu::interrupt_window();
        core::hint::spin_loop();
    }
    let elapsed = rdtsc().wrapping_sub(start);
    lapic.stop_timer();
    let taken = cpu::ticks() - before;
    let micros = elapsed.saturating_mul(1_000_000) / tsc_hz.max(1);
    let expected = PROOF_TICKS * 1_000_000 / HZ;
    // Never faster than programmed (at least 95 % of the nominal time: a calibration error
    // shows here), and at most 50 % slower (an emulator may coalesce late expiries, but a
    // gross error, such as a wrong divisor, still fails). Physical hardware qualification
    // will tighten the upper bound (CLAUDE.md §74).
    let in_range = micros >= expected * 95 / 100 && micros <= expected * 3 / 2;
    if taken < PROOF_TICKS || !in_range {
        let _ = writeln!(
            com1,
            "origin-kernel: {taken} interrupts in {micros} us, expected {PROOF_TICKS} in {expected} us: APIC timer is wrong"
        );
        return None;
    }
    let _ = writeln!(
        com1,
        "origin-kernel: APIC timer: {taken} interrupts at {HZ} Hz in {} ms (initial count {count})",
        micros / 1000
    );
    PERIOD.store(count, core::sync::atomic::Ordering::Release);
    Some(tsc_hz)
}

/// Measures the APIC timer and the TSC over a PIT channel-2 one-shot. Returns their rates
/// in Hz, or `None` if the PIT never reached terminal count.
fn calibrate(lapic: &X2Apic, ports: &mut LegacyPorts) -> Option<(u64, u64)> {
    let pit = apic::pit_count(CALIBRATION_MICROS)?;
    lapic.write(ApicWrite::TimerDivide, apic::divide_config(DIVISOR)?);
    lapic.stop_timer();
    // Gate low and speaker off while programming channel 2 for mode 0.
    let port61 = ports.read(apic::PIT_GATE_PORT) & !0b11;
    ports.write(apic::PIT_GATE_PORT, port61);
    ports.write(apic::PIT_COMMAND, apic::PIT_CHANNEL2_MODE0);
    ports.write(apic::PIT_CHANNEL2, (pit & 0xFF) as u8);
    ports.write(apic::PIT_CHANNEL2, (pit >> 8) as u8);
    // Start the three clocks together: the PIT gate, the APIC count-down, the TSC.
    ports.write(apic::PIT_GATE_PORT, port61 | 1);
    lapic.write(ApicWrite::TimerInitial, u64::from(u32::MAX));
    let tsc0 = rdtsc();
    let mut polls = 0u32;
    // OUT2 (port 0x61 bit 5) rises at terminal count.
    while ports.read(apic::PIT_GATE_PORT) & 0x20 == 0 {
        polls += 1;
        if polls == PIT_POLL_LIMIT {
            lapic.stop_timer();
            return None;
        }
    }
    let tsc1 = rdtsc();
    let current = lapic.read(ApicRead::TimerCurrent);
    lapic.stop_timer();
    ports.write(apic::PIT_GATE_PORT, port61);
    let nanos = apic::pit_nanos(pit);
    let apic_ticks = u64::from(u32::MAX).checked_sub(current)?;
    Some((
        apic::rate(apic_ticks, nanos)?,
        apic::rate(tsc1.wrapping_sub(tsc0), nanos)?,
    ))
}
