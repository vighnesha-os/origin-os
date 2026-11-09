//! The kernel's clocks (CLAUDE.md §68): a monotonic clock in nanoseconds since kernel
//! entry, read from the TSC on any processor, and a UTC wall clock set at boot from the
//! CMOS RTC and advanced by the monotonic clock.
//!
//! At boot, on the boot processor, [`init`]:
//! 1. reads the ACPI FADT: whether the platform has a CMOS RTC, and where its century
//!    register is;
//! 2. times one RTC second on the TSC, an oscillator independent of the PIT that
//!    calibrated the TSC. An update is seen by the seconds register changing, and located
//!    between the polls around it ([`rtc::UpdateWatcher`]), so a virtual CPU descheduled
//!    across QEMU's 244 µs update-in-progress pulse loses the pulse's precision, not the
//!    edge. A pair of edges decides only if its whole interval of uncertainty is inside
//!    or outside 1 % of the PIT rate ([`rtc::SecondMeter`]); a pair too vague to decide
//!    gives way to the next edge, within a 3.2 s budget (the two seconds two edges need,
//!    and one spare for an edge a stall has spoiled);
//! 3. chooses the TSC rate: CPUID leaf 15h's if enumerated and within 1 % of the PIT
//!    calibration; else the RTC second's, if within 1 %; else the PIT calibration;
//! 4. reads the RTC just after the sharpest edge, so the wall clock is right to within
//!    that edge's uncertainty (and, on real hardware, the 1984 µs update cycle), not to a
//!    second.
//!
//! [`prove_consistent`] then checks, on every online processor at once, that ordered TSC
//! readings never go backwards from one processor to the next (cross-CPU time
//! consistency). Nothing depends on that being true, because neither Intel nor AMD promises
//! it: every reading of the monotonic clock passes through a floor shared by all processors
//! ([`MonotonicFloor`], ADR-0012), so no caller on any processor can be given less than a
//! caller has been given before, whatever the TSCs do. [`prove_monotonic`] proves that, and
//! shows on a counter skewed on purpose that a clock without a floor steps back.
//!
//! The TSC is the clock source on every supported platform, whose TSCs are invariant
//! (CPUID 8000_0007h EDX bit 8: constant rate in every P-, C- and T-state). Without that
//! bit (QEMU's TCG does not report it) the clock still runs and still never steps back, but
//! its rate is only what the boot measured, and the boot report says so.
#![forbid(unsafe_code)]

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::acpi;
use crate::clock::{self, NANOS_PER_SECOND, Scale};
use crate::hpet;
use crate::rtc::{self, DateTime, Registers, SecondMeasurement, SecondVerdict, register};
use origin_frame::addr::PhysAddr;
use origin_frame::mmio::MmioRegion;
use origin_frame::monotonic::monotonic_reading;
use origin_frame::sync::SpinLock;
use origin_frame::sync::floor::MonotonicFloor;

use crate::console::Console;
use crate::cpu::{self, rdtsc};
use origin_frame::port;

/// Unix time in nanoseconds at monotonic zero; 0 while unknown.
static BOOT_WALL: AtomicU64 = AtomicU64::new(0);

pub(crate) use origin_frame::monotonic::{entry_tsc, mark_entry, monotonic_nanos, tsc_hz};

/// Unix time in nanoseconds, or `None` if the wall clock was never set.
pub(crate) fn wall_nanos() -> Option<u64> {
    let base = BOOT_WALL.load(Ordering::Relaxed);
    (base != 0).then(|| base.saturating_add(monotonic_nanos()))
}

/// Whether the TSC runs at a constant rate in every power state (CPUID 8000_0007h EDX
/// bit 8).
fn invariant_tsc() -> bool {
    cpu::cpuid(0x8000_0000)[0] >= 0x8000_0007 && cpu::cpuid(0x8000_0007)[3] & (1 << 8) != 0
}

/// Whether RDTSCP exists (CPUID 8000_0001h EDX bit 27).
fn rdtscp_supported() -> bool {
    cpu::cpuid(0x8000_0000)[0] >= 0x8000_0001 && cpu::cpuid(0x8000_0001)[3] & (1 << 27) != 0
}

/// Register B bit 7: updates are inhibited while the clock is being set.
const STATUS_B_SET: u8 = 0x80;
/// How long [`measure_rtc`] may take, in tenths of a second: the two seconds that two
/// edges need, one more for an edge a stall has spoiled, and a margin of 0.2 s. This is
/// the proof's whole cost when the RTC is working: without a spoiled edge, it stops at
/// the second edge, at most two seconds in, as it always has.
const RTC_BUDGET_TENTHS: u64 = 32;
/// The RTC's second must agree with the PIT-calibrated TSC within this many parts per
/// thousand (1 %).
const RTC_TOLERANCE_PPT: u16 = 10;

/// The CMOS real-time clock's registers (the framework's single [`port::Cmos`] handle).
struct Cmos(port::Cmos);

impl Cmos {
    /// Reads RTC register `index` (the NMI-mask bit stays clear).
    fn read(&mut self, index: u8) -> u8 {
        self.0.read(index)
    }

    fn updating(&mut self) -> bool {
        self.read(register::STATUS_A) & rtc::UPDATE_IN_PROGRESS != 0
    }

    /// One poll of the clock: the update-in-progress bit, then the seconds register, in
    /// that order, so that a clear bit can vouch for the seconds read after it, between
    /// two ordered TSC readings.
    fn poll(&mut self, rdtscp: bool) -> rtc::Poll {
        let before = ordered_tsc(rdtscp);
        let updating = self.updating();
        let seconds = self.read(register::SECONDS);
        let after = ordered_tsc(rdtscp);
        rtc::Poll {
            before,
            after,
            updating,
            seconds,
        }
    }

    fn registers(&mut self, century: Option<u8>) -> Registers {
        let seconds = self.read(register::SECONDS);
        let minutes = self.read(register::MINUTES);
        let hours = self.read(register::HOURS);
        let day = self.read(register::DAY);
        let month = self.read(register::MONTH);
        let year = self.read(register::YEAR);
        let century = century.map(|index| self.read(index));
        let status_b = self.read(register::STATUS_B);
        Registers {
            seconds,
            minutes,
            hours,
            day,
            month,
            year,
            century,
            status_b,
        }
    }

    /// Two identical consecutive readings (an update cannot then have split one), or
    /// `None` after a few attempts.
    fn stable_registers(&mut self, century: Option<u8>) -> Option<Registers> {
        let mut last = self.registers(century);
        for _ in 0..4 {
            let next = self.registers(century);
            if next == last {
                return Some(next);
            }
            last = next;
        }
        None
    }
}

/// What timing the RTC's second gave.
struct RtcSecond {
    /// The measurement that decided, or if none did, the tightest one (`None`: fewer than
    /// two updates were seen).
    second: Option<SecondMeasurement>,
    /// The TSC just after the sharpest update seen, and the clock as read there.
    reading: Option<(u64, Registers)>,
}

/// Times the RTC's second on the TSC, which should run at about `hz`, within
/// [`RTC_BUDGET_TENTHS`], and reads the clock just after the sharpest update edge it
/// finds. See [`rtc::UpdateWatcher`] and [`rtc::SecondMeter`] for the method.
fn measure_rtc(cmos: &mut Cmos, hz: u64, century: Option<u8>) -> RtcSecond {
    let rdtscp = rdtscp_supported();
    let mut watcher = rtc::UpdateWatcher::new(cmos.read(register::STATUS_B), hz);
    let mut meter = rtc::SecondMeter::new(hz, RTC_TOLERANCE_PPT);
    let limit = hz.saturating_mul(RTC_BUDGET_TENTHS) / 10;
    let start = ordered_tsc(rdtscp);
    let mut sharpest = u64::MAX;
    let mut reading = None;
    let second = loop {
        let poll = cmos.poll(rdtscp);
        if poll.before.wrapping_sub(start) > limit {
            break meter.tightest();
        }
        let Some(edge) = watcher.observe(poll) else {
            continue;
        };
        // The registers hold the new second now, for almost a full second. A stall may
        // have made this edge vague, and a later, sharper edge gets a better reading.
        if edge.width() < sharpest
            && let Some(regs) = cmos.stable_registers(century)
            && regs.seconds == poll.seconds
        {
            sharpest = edge.width();
            reading = Some((edge.latest, regs));
        }
        if let Some(decided) = meter.add(edge) {
            break Some(decided);
        }
    };
    RtcSecond { second, reading }
}

/// Sets up both clocks from the PIT-calibrated TSC rate `calibrated_hz`, the RTC and the
/// ACPI tables at `rsdp`. Returns whether every check passed.
pub(crate) fn init(com1: &mut Console, calibrated_hz: u64, rsdp: u64) -> bool {
    let fadt = crate::firmware::acpi_table(rsdp, b"FACP")
        .ok()
        .and_then(|t| acpi::parse_fadt(t).ok());
    let century = fadt.and_then(|f| f.century);
    let rtc_present = !fadt.is_some_and(|f| f.cmos_rtc_absent);
    let mut ok = true;
    // One RTC second on the TSC, and a reading of the clock just after an update.
    let measured = rtc_present
        .then(|| port::Cmos::take().map(Cmos))
        .flatten()
        .map(|mut cmos| measure_rtc(&mut cmos, calibrated_hz, century));
    let second = measured.as_ref().and_then(|m| m.second);
    let cpuid_hz = crate::timer::cpuid_tsc_hz();
    let calibrated_by = crate::timer::calibration_source();
    let (hz, source) = match (cpuid_hz, second.map(|s| s.ticks())) {
        (Some(c), _) if clock::rates_agree(c, calibrated_hz, 10) => (c, "CPUID 15h"),
        (_, Some(r)) if clock::rates_agree(r, calibrated_hz, 10) => (r, "one RTC second"),
        _ => (calibrated_hz, calibrated_by),
    };
    let Some(scale) = Scale::new(hz) else {
        let _ = writeln!(com1, "origin-kernel: clock: TSC rate 0: clock failed");
        return false;
    };
    origin_frame::monotonic::publish_scale(scale);
    let _ = writeln!(
        com1,
        "origin-kernel: clock: TSC at {} kHz, from {source}; invariant TSC {}; calibrated against {calibrated_by} at {} kHz; CPUID 15h {}",
        hz / 1000,
        invariant_tsc(),
        calibrated_hz / 1000,
        Khz(cpuid_hz)
    );
    if !invariant_tsc() {
        let _ = writeln!(
            com1,
            "origin-kernel: clock: this processor does not report an invariant TSC (CPUID 8000_0007h EDX bit 8): the monotonic clock still never steps back, but its rate is only what this boot measured"
        );
    }
    // The proof that the calibration is right: an independent oscillator agrees with it.
    match second {
        Some(second) => {
            let micros =
                |ticks: u64| u128::from(ticks) * 1_000_000 / u128::from(calibrated_hz.max(1));
            ok &= second.verdict == SecondVerdict::Confirmed;
            let _ = writeln!(
                com1,
                "origin-kernel: clock: one RTC second lasted {} us on the calibrated TSC (+/- {} us, over {} s){}",
                micros(second.ticks()),
                micros(second.error_ticks()),
                second.seconds,
                match second.verdict {
                    SecondVerdict::Confirmed => "",
                    SecondVerdict::Refuted => ": clock failed",
                    SecondVerdict::Undecided =>
                        ", too uncertain to confirm within 1 %: clock failed",
                }
            );
        }
        None if rtc_present => {
            ok = false;
            let _ = writeln!(
                com1,
                "origin-kernel: clock: the RTC did not complete two updates in {}.{} s: clock failed",
                RTC_BUDGET_TENTHS / 10,
                RTC_BUDGET_TENTHS % 10
            );
        }
        None => {}
    }
    let reading = measured.and_then(|m| m.reading);
    ok &= set_wall_clock(com1, scale, reading, century, rtc_present);
    ok &= prove_hpet(com1, rsdp, scale);
    ok &= prove_pm_timer(com1, rsdp, scale);
    ok
}

/// Attempts at reading a counter paired closely with the TSC: a read is good when its TSC
/// bracket is within [`pair_tight_ticks`]. Rare stalls never need more than a couple.
const PAIR_READS: u32 = 8;

/// How closely, in TSC ticks, a counter reading must be bracketed by the TSC to be used
/// against the HPET's or PM timer's 2 % over 20 ms: within 20 µs, a tenth of that error.
fn pair_tight_ticks(scale: Scale) -> u64 {
    scale.hz() / 50_000
}

/// Reads the HPET's 64-bit register at `offset`.
fn hpet_read(regs: MmioRegion, offset: u64) -> u64 {
    regs.read64(offset as usize)
}

/// Writes the HPET's 64-bit register at `offset`: only General Configuration is written here,
/// which affects only the HPET.
fn hpet_write(regs: MmioRegion, offset: u64, value: u64) {
    regs.write64(offset as usize, value);
}

/// Discovers the HPET through ACPI, enables its main counter, and checks it advances at the
/// rate its capabilities register declares, consistent with the TSC (CLAUDE.md §68). No
/// HPET is reported and passes.
fn prove_hpet(com1: &mut Console, rsdp: u64, scale: Scale) -> bool {
    let Ok(table) = crate::firmware::acpi_table(rsdp, b"HPET") else {
        let _ = writeln!(com1, "origin-kernel: HPET: no table");
        return true;
    };
    let info = match hpet::parse(table) {
        Ok(i) => i,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: HPET: invalid table: {e:?}: HPET failed"
            );
            return false;
        }
    };
    let virt = match crate::vm::map_mmio(PhysAddr::new(info.base), 0x1000) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(
                com1,
                "origin-kernel: HPET: cannot map {:#x}: {e:?}: HPET failed",
                info.base
            );
            return false;
        }
    };
    let cap = hpet_read(virt, hpet::register::CAPABILITIES);
    let Some(declared_hz) = hpet::frequency_hz(cap) else {
        let _ = writeln!(
            com1,
            "origin-kernel: HPET: capabilities {cap:#x} give no usable period: HPET failed"
        );
        return false;
    };
    let period = hpet::period_fs(cap);
    // Enable the main counter.
    let config = hpet_read(virt, hpet::register::CONFIG);
    hpet_write(virt, hpet::register::CONFIG, config | hpet::ENABLE_CNF);
    // Measure over about 20 ms of the TSC, each HPET read paired closely with the TSC.
    let read = move || {
        let before = rdtsc();
        let value = hpet_read(virt, hpet::register::COUNTER);
        (before, value, rdtsc())
    };
    let tight = pair_tight_ticks(scale);
    let (c0, t0) = clock::closest_read(PAIR_READS, tight, read);
    let window = scale.hz() / 50;
    while rdtsc().wrapping_sub(t0) < window {
        core::hint::spin_loop();
    }
    let (c1, t1) = clock::closest_read(PAIR_READS, tight, read);
    let hpet_nanos = hpet::ticks_to_nanos(c1.wrapping_sub(c0), period);
    let tsc_nanos = scale.nanos(t1.wrapping_sub(t0));
    // The counter must advance (not frozen) and agree with the TSC within 2 %.
    let advanced = c1 != c0;
    let agree = advanced && hpet_nanos != 0 && hpet::agree(hpet_nanos, tsc_nanos, 20);
    let _ = writeln!(
        com1,
        "origin-kernel: HPET: at {:#x}, {} MHz counter (period {period} fs); {} ms elapsed by the HPET, {} ms by the TSC{}",
        info.base,
        declared_hz / 1_000_000,
        hpet_nanos / 1_000_000,
        tsc_nanos / 1_000_000,
        if agree { "" } else { ": HPET failed" }
    );
    agree
}

/// Cross-checks the ACPI power-management timer (a 3.579545 MHz counter at a fixed I/O
/// port, from the FADT) against the TSC, like the HPET (CLAUDE.md §68). No PM timer is
/// reported and passes.
fn prove_pm_timer(com1: &mut Console, rsdp: u64, scale: Scale) -> bool {
    let fadt = crate::firmware::acpi_table(rsdp, b"FACP")
        .ok()
        .and_then(|t| acpi::parse_fadt(t).ok());
    let Some((port, wide)) = fadt.map(|f| (f.pm_timer_port, f.pm_timer_32bit)) else {
        let _ = writeln!(com1, "origin-kernel: PM timer: no FADT");
        return true;
    };
    let Some(port) = port else {
        let _ = writeln!(com1, "origin-kernel: PM timer: the FADT provides none");
        return true;
    };
    let mask: u32 = if wide { 0xFFFF_FFFF } else { 0x00FF_FFFF };
    // The FADT's PM_TMR_BLK: a read-only 32-bit timer port. A first claim of it succeeds.
    let Some(timer) = origin_frame::port::PortRange::claim(port, 4) else {
        let _ = writeln!(
            com1,
            "origin-kernel: PM timer: its port {port:#x} is not available: PM timer failed"
        );
        return false;
    };
    let read = move || {
        let before = rdtsc();
        let value = u64::from(timer.read32(0) & mask);
        (before, value, rdtsc())
    };
    let tight = pair_tight_ticks(scale);
    let (c0, t0) = clock::closest_read(PAIR_READS, tight, read);
    let window = scale.hz() / 50; // ~20 ms
    while rdtsc().wrapping_sub(t0) < window {
        core::hint::spin_loop();
    }
    let (c1, t1) = clock::closest_read(PAIR_READS, tight, read);
    let pm_ticks = c1.wrapping_sub(c0) as u32 & mask; // handles a single wrap
    let pm_nanos =
        u64::try_from(u128::from(pm_ticks) * 1_000_000_000 / u128::from(acpi::PM_TIMER_HZ))
            .unwrap_or(u64::MAX);
    let tsc_nanos = scale.nanos(t1.wrapping_sub(t0));
    let advanced = pm_ticks != 0;
    let agree = advanced && hpet::agree(pm_nanos, tsc_nanos, 20);
    let _ = writeln!(
        com1,
        "origin-kernel: PM timer: {}-bit at port {port:#x}, 3.579545 MHz; {} ms elapsed by the PM timer, {} ms by the TSC{}",
        if wide { 32 } else { 24 },
        pm_nanos / 1_000_000,
        tsc_nanos / 1_000_000,
        if agree { "" } else { ": PM timer failed" }
    );
    agree
}

/// Sets the wall clock from the RTC reading taken at TSC `t1`, just after an update.
fn set_wall_clock(
    com1: &mut Console,
    scale: Scale,
    reading: Option<(u64, Registers)>,
    century: Option<u8>,
    rtc_present: bool,
) -> bool {
    if !rtc_present {
        let _ = writeln!(
            com1,
            "origin-kernel: wall clock: unknown: the FADT says the platform has no CMOS RTC"
        );
        return true;
    }
    let Some((t1, regs)) = reading else {
        let _ = writeln!(
            com1,
            "origin-kernel: wall clock: no stable RTC reading: clock failed"
        );
        return false;
    };
    let when = match rtc::decode(&regs) {
        Ok(when) => when,
        Err(field) => {
            let _ = writeln!(
                com1,
                "origin-kernel: wall clock: the RTC's {field:?} is invalid ({regs:?}): clock failed"
            );
            return false;
        }
    };
    // Unix time in nanoseconds must fit in 64 bits (until 2554-07-21); a later year is a
    // corrupt RTC, not a date.
    let Some(unix) = when
        .unix_seconds()
        .and_then(|s| s.checked_mul(NANOS_PER_SECOND))
    else {
        let _ = writeln!(
            com1,
            "origin-kernel: wall clock: the RTC says {when}, beyond the clock's range: clock failed"
        );
        return false;
    };
    let at = scale.nanos(t1.saturating_sub(entry_tsc()));
    let base = unix.saturating_sub(at);
    BOOT_WALL.store(base, Ordering::Relaxed);
    let now = wall_nanos().unwrap_or(0);
    let shown = DateTime::from_unix_seconds(now / NANOS_PER_SECOND).unwrap_or(when);
    let _ = writeln!(
        com1,
        "origin-kernel: wall clock: {shown} (Unix time {}.{:03}), from the CMOS RTC at an update edge ({}, {}-hour, {})",
        now / NANOS_PER_SECOND,
        now % NANOS_PER_SECOND / 1_000_000,
        if regs.status_b & rtc::MODE_BINARY != 0 {
            "binary"
        } else {
            "BCD"
        },
        if regs.status_b & rtc::MODE_24_HOUR != 0 {
            24
        } else {
            12
        },
        CenturyRegister(century)
    );
    if regs.status_b & STATUS_B_SET != 0 {
        let _ = writeln!(
            com1,
            "origin-kernel: wall clock: the RTC's updates are inhibited (register B SET)"
        );
    }
    true
}

/// A rate for the boot report: `2400000 kHz`, or `not enumerated`.
struct Khz(Option<u64>);

impl core::fmt::Display for Khz {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(hz) => write!(f, "{} kHz", hz / 1000),
            None => f.write_str("not enumerated"),
        }
    }
}

/// The FADT's century register for the boot report: `century register 0x32`.
struct CenturyRegister(Option<u8>);

impl core::fmt::Display for CenturyRegister {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(index) => write!(f, "century register {index:#04x}"),
            None => f.write_str("no century register"),
        }
    }
}

/// The last ordered TSC reading any processor took, and the x2APIC ID of the processor
/// that took it (`u64::MAX`: none yet).
static TURN: SpinLock<(u64, u64)> = SpinLock::new((0, u64::MAX));
/// Readings that were below the one before them.
static BACKWARDS: AtomicU64 = AtomicU64::new(0);
/// The largest such step back, in TSC ticks.
static WORST: AtomicU64 = AtomicU64::new(0);
/// Readings taken right after another processor's.
static HANDOFFS: AtomicU64 = AtomicU64::new(0);
static READINGS: AtomicU64 = AtomicU64::new(0);
/// Readings a floor gave back higher than they were read.
static HELD: AtomicU64 = AtomicU64::new(0);
/// Turn-takers on the application processors that are ready, and that have finished.
static READY: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicU64 = AtomicU64::new(0);
/// The TSC value at which every turn-taker stops; 0 until the boot processor starts them.
static UNTIL: AtomicU64 = AtomicU64::new(0);
/// The TSC value at which they stop whatever else has happened.
static LATEST: AtomicU64 = AtomicU64::new(0);
/// How many handoffs a run keeps going for, up to [`LATEST`].
static WANTED_HANDOFFS: AtomicU64 = AtomicU64::new(0);
/// Handoffs needed for the check to mean anything: the processors must really have
/// interleaved.
const MIN_HANDOFFS: u64 = 100;
/// Handoffs a control run keeps going for: it only has to see the skewed counters disagree.
const CONTROL_HANDOFFS: u64 = 16;
/// The floor the control runs use, so that their skew never reaches the real clock's.
static CONTROL_FLOOR: MonotonicFloor = MonotonicFloor::new();

/// What the processors read and compare in one run of turns (`repr(u64)`: a turn-taker
/// thread gets it as its argument).
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum Reading {
    /// Ordered TSC readings: do the counters themselves agree?
    Tsc,
    /// The monotonic clock: does what callers are given ever go back?
    Clock,
    /// The control: each processor's ordered TSC, skewed by a second of ticks per processor
    /// index, with no floor. It must step back, or the proof cannot see what it looks for.
    Skewed,
    /// The same skewed counters through [`CONTROL_FLOOR`]. It must never step back.
    SkewedThroughFloor,
}

impl Reading {
    /// The reading a turn-taker's argument names.
    fn of(argument: u64) -> Self {
        match argument {
            0 => Self::Tsc,
            1 => Self::Clock,
            2 => Self::Skewed,
            _ => Self::SkewedThroughFloor,
        }
    }

    /// What processor `me` reads at TSC `now`. Counts in [`HELD`] a reading a floor raised.
    fn value(self, now: u64, me: u64) -> u64 {
        let skewed = || now.wrapping_add(me.wrapping_mul(tsc_hz()));
        let through = |counted: u64, given: u64| {
            HELD.fetch_add(u64::from(given > counted), Ordering::Relaxed);
            given
        };
        match self {
            Self::Tsc => now,
            Self::Clock => {
                monotonic_reading().map_or(0, |(counted, given)| through(counted, given))
            }
            Self::Skewed => skewed(),
            Self::SkewedThroughFloor => {
                let counted = skewed();
                through(counted, CONTROL_FLOOR.advance(counted))
            }
        }
    }
}

/// The TSC, read only once every earlier instruction has executed (RDTSCP, else LFENCE and
/// RDTSC): the framework's `cpu::ordered_tsc`.
fn ordered_tsc(rdtscp: bool) -> u64 {
    cpu::ordered_tsc(rdtscp)
}

/// Reads `reading` under [`TURN`]'s lock until [`UNTIL`], comparing each value with the one
/// before it, whichever processor took that one. While fewer than `min_handoffs` handoffs have
/// happened it keeps going, to [`LATEST`] at most: on a loaded host the virtual CPUs rarely
/// run at the same moment.
fn take_turns(me: u64, min_handoffs: u64, reading: Reading) {
    let rdtscp = rdtscp_supported();
    let until = UNTIL.load(Ordering::Acquire);
    let latest = LATEST.load(Ordering::Relaxed);
    loop {
        let finished = cpu::without_interrupts(|| {
            let mut turn = TURN.lock();
            let now = ordered_tsc(rdtscp);
            let value = reading.value(now, me);
            let (last, by) = *turn;
            if value < last {
                BACKWARDS.fetch_add(1, Ordering::Relaxed);
                WORST.fetch_max(last - value, Ordering::Relaxed);
            }
            if by != me && by != u64::MAX {
                HANDOFFS.fetch_add(1, Ordering::Relaxed);
            }
            READINGS.fetch_add(1, Ordering::Relaxed);
            *turn = (value, me);
            now >= until && (HANDOFFS.load(Ordering::Relaxed) >= min_handoffs || now >= latest)
        });
        if finished {
            return;
        }
        // A pause outside the lock, so the other processors get their turns.
        for _ in 0..32 {
            core::hint::spin_loop();
        }
    }
}

/// An application processor's turn-taker thread; its argument names what to read.
extern "sysv64" fn turn_taker(reading: u64) -> ! {
    READY.fetch_add(1, Ordering::AcqRel);
    while UNTIL.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    take_turns(
        cpu::this_cpu().get() as u64,
        WANTED_HANDOFFS.load(Ordering::Relaxed),
        Reading::of(reading),
    );
    DONE.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
}

/// What one run of turns saw.
struct Turns {
    readings: u64,
    handoffs: u64,
    backwards: u64,
    worst: u64,
    held: u64,
    /// Every turn-taker finished.
    done: bool,
}

/// One run of turns with every online processor reading `reading`, for `window` ticks and on
/// to 500 ms until `wanted` handoffs have happened. `None`, reported, if the turn-takers cannot
/// be started. Runs from the boot flow with every scheduler active, after any earlier run has
/// finished: nothing else reads or writes this state.
fn run_turns(
    com1: &mut Console,
    cpus: &[cpu::CpuIndex],
    bsp: cpu::CpuIndex,
    tsc_hz: u64,
    reading: Reading,
    window: u64,
    wanted: u64,
) -> Option<Turns> {
    *TURN.lock() = (0, u64::MAX);
    for counter in [
        &BACKWARDS, &WORST, &HANDOFFS, &READINGS, &HELD, &READY, &DONE, &UNTIL,
    ] {
        counter.store(0, Ordering::Release);
    }
    WANTED_HANDOFFS.store(wanted, Ordering::Relaxed);
    let mut aps = 0u64;
    for &id in cpus.iter().filter(|&&id| id != bsp) {
        if crate::sched::spawn(id, turn_taker, reading as u64).is_err() {
            // Release any that did start: they would otherwise spin for the whole boot.
            UNTIL.store(1, Ordering::Release);
            let _ = writeln!(
                com1,
                "origin-kernel: clock: cannot spawn on CPU {id}: clock failed"
            );
            return None;
        }
        aps += 1;
    }
    crate::sched::yield_until(|| READY.load(Ordering::Acquire) == aps, tsc_hz, 2);
    if READY.load(Ordering::Acquire) != aps {
        UNTIL.store(1, Ordering::Release);
        let _ = writeln!(
            com1,
            "origin-kernel: clock: turn-takers did not start: clock failed"
        );
        return None;
    }
    let start = rdtsc();
    LATEST.store(start.saturating_add(tsc_hz / 2), Ordering::Relaxed);
    UNTIL.store(start.saturating_add(window), Ordering::Release);
    take_turns(
        bsp.get() as u64,
        if cpus.len() < 2 { 0 } else { wanted },
        reading,
    );
    crate::sched::yield_until(|| DONE.load(Ordering::Acquire) == aps, tsc_hz, 2);
    Some(Turns {
        readings: READINGS.load(Ordering::Relaxed),
        handoffs: HANDOFFS.load(Ordering::Relaxed),
        backwards: BACKWARDS.load(Ordering::Relaxed),
        worst: WORST.load(Ordering::Relaxed),
        held: HELD.load(Ordering::Relaxed),
        done: DONE.load(Ordering::Acquire) == aps,
    })
}

/// Proves that the TSC readings of every online processor are consistent: for 100 ms
/// (and on, to 500 ms, until they have really interleaved) all of them take ordered
/// readings in turn, and no reading may be below the one taken just before it on another
/// processor. Runs from the boot flow with every scheduler active.
pub(crate) fn prove_consistent(
    com1: &mut Console,
    cpus: &[cpu::CpuIndex],
    bsp: cpu::CpuIndex,
    tsc_hz: u64,
) -> bool {
    let Some(run) = run_turns(
        com1,
        cpus,
        bsp,
        tsc_hz,
        Reading::Tsc,
        tsc_hz / 10,
        MIN_HANDOFFS,
    ) else {
        return false;
    };
    let interleaved = cpus.len() < 2 || run.handoffs >= MIN_HANDOFFS;
    let ok = run.done && run.backwards == 0 && interleaved;
    let _ = writeln!(
        com1,
        "origin-kernel: clock: TSC consistent across {} processors: {} ordered readings, {} handoffs between processors, {} backwards (worst {} ticks){}",
        cpus.len(),
        run.readings,
        run.handoffs,
        run.backwards,
        run.worst,
        if ok { "" } else { ": clock failed" }
    );
    ok
}

/// Reads each processor makes in the cost measurement: long enough to see through QEMU's
/// TSC, which on the development host moves in 42-tick steps, and about a millisecond on
/// real hardware.
const COST_READS: u64 = 1 << 16;
/// The per-processor means of a cost measurement, summed.
static COST_TICKS: AtomicU64 = AtomicU64::new(0);

/// The mean ticks one call of `read` takes, over [`COST_READS`] calls with interrupts off.
fn ticks_per_read(read: impl Fn() -> u64) -> u64 {
    let rdtscp = rdtscp_supported();
    cpu::without_interrupts(|| {
        let start = ordered_tsc(rdtscp);
        for _ in 0..COST_READS {
            core::hint::black_box(read());
        }
        ordered_tsc(rdtscp).wrapping_sub(start) / COST_READS
    })
}

/// An application processor's thread in the cost measurement.
extern "sysv64" fn cost_reader(_: u64) -> ! {
    READY.fetch_add(1, Ordering::AcqRel);
    while UNTIL.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    COST_TICKS.fetch_add(ticks_per_read(monotonic_nanos), Ordering::Relaxed);
    DONE.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
}

/// Measures and reports what a read of the monotonic clock costs (CLAUDE.md §66, rule 21): on
/// this processor alone, beside a bare RDTSC and the same read without the floor; and with
/// every processor reading at once, the case the shared floor makes expensive. This reports;
/// it decides nothing.
fn report_read_cost(com1: &mut Console, cpus: &[cpu::CpuIndex], bsp: cpu::CpuIndex, tsc_hz: u64) {
    let entry = entry_tsc();
    let bare = ticks_per_read(rdtsc);
    let unfloored = origin_frame::monotonic::published_scale().map_or(0, |scale| {
        ticks_per_read(|| scale.nanos(rdtsc().saturating_sub(entry)))
    });
    let floored = ticks_per_read(monotonic_nanos);
    let mut started = true;
    let mut aps = 0;
    for counter in [&READY, &DONE, &UNTIL, &COST_TICKS] {
        counter.store(0, Ordering::Release);
    }
    for &id in cpus.iter().filter(|&&id| id != bsp) {
        started &= crate::sched::spawn(id, cost_reader, 0).is_ok();
        aps += 1;
    }
    crate::sched::yield_until(|| READY.load(Ordering::Acquire) == aps, tsc_hz, 2);
    started &= READY.load(Ordering::Acquire) == aps;
    // Whatever happened, release those that did start: they would otherwise spin for the
    // whole boot.
    UNTIL.store(1, Ordering::Release);
    if !started {
        let _ = writeln!(
            com1,
            "origin-kernel: clock: reading it costs {floored} ticks alone (RDTSC {bare}, without the floor {unfloored}); processors could not be started to read it at once"
        );
        return;
    }
    COST_TICKS.fetch_add(ticks_per_read(monotonic_nanos), Ordering::Relaxed);
    crate::sched::yield_until(|| DONE.load(Ordering::Acquire) == aps, tsc_hz, 2);
    let finished = DONE.load(Ordering::Acquire) == aps;
    let together = COST_TICKS.load(Ordering::Relaxed) / (aps + 1);
    let _ = writeln!(
        com1,
        "origin-kernel: clock: reading it costs {floored} ticks alone (RDTSC {bare}, without the floor {unfloored}) and {together} ticks with {} processors reading at once{}",
        aps + 1,
        if finished {
            ""
        } else {
            " (some processors did not finish)"
        }
    );
}

/// Proves the monotonic clock's contract (ADR-0012): every online processor reads
/// [`monotonic_nanos`] in turn, and no value may be below the one before it, whichever
/// processor took that one.
///
/// Then the same check on counters made to disagree: each processor's TSC offset by a second
/// of ticks per processor index. Read as they are they must step back, or this proof could not
/// see what it looks for; read through a floor of their own they must not. The skew never
/// reaches the real clock, and a machine with one processor has nothing to compare.
pub(crate) fn prove_monotonic(
    com1: &mut Console,
    cpus: &[cpu::CpuIndex],
    bsp: cpu::CpuIndex,
    tsc_hz: u64,
) -> bool {
    let window = tsc_hz / 50;
    let run = |com1: &mut Console, reading, wanted| {
        run_turns(com1, cpus, bsp, tsc_hz, reading, window, wanted)
    };
    let Some(clock) = run(com1, Reading::Clock, MIN_HANDOFFS) else {
        return false;
    };
    let controls = if cpus.len() >= 2 {
        let (Some(bare), Some(floored)) = (
            run(com1, Reading::Skewed, CONTROL_HANDOFFS),
            run(com1, Reading::SkewedThroughFloor, CONTROL_HANDOFFS),
        ) else {
            return false;
        };
        Some((bare, floored))
    } else {
        None
    };
    let interleaved = controls.is_none() || clock.handoffs >= MIN_HANDOFFS;
    let ok = clock.done
        && clock.backwards == 0
        && interleaved
        && controls.as_ref().is_none_or(|(bare, floored)| {
            bare.done
                && bare.backwards > 0
                && floored.done
                && floored.backwards == 0
                && floored.held > 0
        });
    let outcome = if ok { "" } else { ": clock failed" };
    let _ = match controls {
        Some((bare, floored)) => writeln!(
            com1,
            "origin-kernel: clock: monotonic across {} processors: {} readings, {} handoffs between processors, {} backwards, {} held at the floor; control, counters skewed by a second per processor: {} backwards without a floor, {} through one ({} held){outcome}",
            cpus.len(),
            clock.readings,
            clock.handoffs,
            clock.backwards,
            clock.held,
            bare.backwards,
            floored.backwards,
            floored.held,
        ),
        None => writeln!(
            com1,
            "origin-kernel: clock: monotonic across {} processors: {} readings, {} handoffs between processors, {} backwards, {} held at the floor; the controls need a second processor{outcome}",
            cpus.len(),
            clock.readings,
            clock.handoffs,
            clock.backwards,
            clock.held,
        ),
    };
    report_read_cost(com1, cpus, bsp, tsc_hz);
    ok
}
