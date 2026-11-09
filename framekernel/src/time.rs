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
//! consistency): the monotonic clock is only monotonic across processors if their TSCs
//! agree.
//!
//! The TSC is the clock source on every supported platform, whose TSCs are invariant
//! (CPUID 8000_0007h EDX bit 8: constant rate in every P-, C- and T-state). Without that
//! bit (QEMU's TCG does not report it) the clock still runs, and the boot report says so.

use core::arch::asm;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use framekernel::acpi;
use framekernel::clock::{self, NANOS_PER_SECOND, Scale};
use framekernel::hpet;
use framekernel::rtc::{self, DateTime, Registers, SecondMeasurement, SecondVerdict, register};
use origin_frame::sync::SpinLock;

use crate::console::Console;
use crate::cpu::{self, rdtsc};
use crate::{inb, outb};

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

/// The TSC rate CPUID leaf 15h enumerates, if it does.
fn cpuid_tsc_hz() -> Option<u64> {
    if cpu::cpuid(0)[0] < 0x15 {
        return None;
    }
    let [eax, ebx, ecx, _] = cpu::cpuid(0x15);
    clock::cpuid_tsc_hz(eax, ebx, ecx)
}

/// Whether RDTSCP exists (CPUID 8000_0001h EDX bit 27).
fn rdtscp_supported() -> bool {
    cpu::cpuid(0x8000_0000)[0] >= 0x8000_0001 && cpu::cpuid(0x8000_0001)[3] & (1 << 27) != 0
}

/// CMOS index and data ports.
const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;
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

/// The CMOS RTC, through ports 70h and 71h.
struct Cmos(());

impl Cmos {
    /// # Safety
    ///
    /// CPL 0, and nothing else drives ports 70h–71h while the value lives (only the boot
    /// processor reads the clock, at boot). Bit 7 of the index port masks NMIs on some
    /// chipsets; every index written here has it clear, so NMIs stay enabled.
    unsafe fn new() -> Self {
        Self(())
    }

    /// Reads CMOS register `index` (below 80h). No register read here has a side effect:
    /// register C, whose read acknowledges RTC interrupts, is never read.
    fn read(&mut self, index: u8) -> u8 {
        // SAFETY: the constructor's contract; selecting a register only latches an index.
        unsafe { outb(CMOS_INDEX, index & 0x7F) };
        // SAFETY: as above; the read returns the selected register and cannot affect
        // memory.
        unsafe { inb(CMOS_DATA) }
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
    let measured = rtc_present.then(|| {
        // SAFETY: CPL 0, the boot processor, interrupts disabled; no other code uses the
        // CMOS ports.
        let mut cmos = unsafe { Cmos::new() };
        measure_rtc(&mut cmos, calibrated_hz, century)
    });
    let second = measured.as_ref().and_then(|m| m.second);
    let cpuid_hz = cpuid_tsc_hz();
    let (hz, source) = match (cpuid_hz, second.map(|s| s.ticks())) {
        (Some(c), _) if clock::rates_agree(c, calibrated_hz, 10) => (c, "CPUID 15h"),
        (_, Some(r)) if clock::rates_agree(r, calibrated_hz, 10) => (r, "one RTC second"),
        _ => (calibrated_hz, "the PIT calibration"),
    };
    let Some(scale) = Scale::new(hz) else {
        let _ = writeln!(com1, "origin-kernel: clock: TSC rate 0: clock failed");
        return false;
    };
    origin_frame::monotonic::publish_scale(scale);
    let _ = writeln!(
        com1,
        "origin-kernel: clock: TSC at {} kHz, from {source}; invariant TSC {}; PIT calibration {} kHz; CPUID 15h {}",
        hz / 1000,
        invariant_tsc(),
        calibrated_hz / 1000,
        Khz(cpuid_hz)
    );
    // The proof that the calibration is right: an independent oscillator agrees with it.
    match second {
        Some(second) => {
            let micros =
                |ticks: u64| u128::from(ticks) * 1_000_000 / u128::from(calibrated_hz.max(1));
            ok &= second.verdict == SecondVerdict::Confirmed;
            let _ = writeln!(
                com1,
                "origin-kernel: clock: one RTC second lasted {} us on the PIT-calibrated TSC (+/- {} us, over {} s){}",
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

/// Reads the HPET's 64-bit register at `virt + offset`.
fn hpet_read(virt: u64, offset: u64) -> u64 {
    let p = core::ptr::with_exposed_provenance::<u64>((virt + offset) as usize);
    // SAFETY: `virt` maps the HPET's registers uncached (just mapped); `offset` names a
    // 64-bit-aligned register inside the 1 KiB block, and a register read has no memory
    // effect.
    unsafe { p.read_volatile() }
}

/// Writes the HPET's 64-bit register at `virt + offset`.
fn hpet_write(virt: u64, offset: u64, value: u64) {
    let p = core::ptr::with_exposed_provenance_mut::<u64>((virt + offset) as usize);
    // SAFETY: as for `hpet_read`; the only register written here is General Configuration,
    // which affects only the HPET.
    unsafe { p.write_volatile(value) };
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
    let virt = match crate::vm::map_mmio(info.base, 0x1000) {
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
    let read = move || {
        let before = rdtsc();
        // SAFETY: CPL 0, boot processor; `port` is the FADT's PM_TMR_BLK, a read-only
        // 32-bit timer port that cannot affect memory.
        let value = u64::from((unsafe { crate::ind(port) }) & mask);
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
/// Turn-takers on the application processors that are ready, and that have finished.
static READY: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicU64 = AtomicU64::new(0);
/// The TSC value at which every turn-taker stops; 0 until the boot processor starts them.
static UNTIL: AtomicU64 = AtomicU64::new(0);
/// The TSC value at which they stop whatever else has happened.
static LATEST: AtomicU64 = AtomicU64::new(0);
/// Handoffs needed for the check to mean anything: the processors must really have
/// interleaved.
const MIN_HANDOFFS: u64 = 100;

/// The TSC, read only once every earlier instruction has executed, so a reading cannot be
/// taken before the lock that orders it is held: RDTSCP (Intel SDM Vol. 2B, RDTSCP; AMD
/// APM Vol. 3), else LFENCE then RDTSC.
fn ordered_tsc(rdtscp: bool) -> u64 {
    let (low, high): (u32, u32);
    if rdtscp {
        // SAFETY: RDTSCP exists (the caller checked CPUID); it writes EDX:EAX and ECX
        // (IA32_TSC_AUX, unused here) and reads no memory.
        unsafe {
            asm!("rdtscp", out("eax") low, out("edx") high, out("ecx") _, options(nomem, nostack, preserves_flags));
        }
    } else {
        // SAFETY: LFENCE and RDTSC exist on every x86-64 processor and read no memory.
        unsafe {
            asm!("lfence", "rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
        }
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Takes ordered TSC readings under [`TURN`]'s lock until [`UNTIL`], comparing each with
/// the reading before it, whichever processor took that one. While fewer than
/// `min_handoffs` handoffs have happened it keeps going, to [`LATEST`] at most: on a loaded
/// host the virtual CPUs rarely run at the same moment.
fn take_turns(me: u64, min_handoffs: u64) {
    let rdtscp = rdtscp_supported();
    let until = UNTIL.load(Ordering::Acquire);
    let latest = LATEST.load(Ordering::Relaxed);
    loop {
        let finished = cpu::without_interrupts(|| {
            let mut turn = TURN.lock();
            let now = ordered_tsc(rdtscp);
            let (last, by) = *turn;
            if now < last {
                BACKWARDS.fetch_add(1, Ordering::Relaxed);
                WORST.fetch_max(last - now, Ordering::Relaxed);
            }
            if by != me && by != u64::MAX {
                HANDOFFS.fetch_add(1, Ordering::Relaxed);
            }
            READINGS.fetch_add(1, Ordering::Relaxed);
            *turn = (now, me);
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

/// An application processor's turn-taker thread.
extern "sysv64" fn turn_taker(_: u64) -> ! {
    READY.fetch_add(1, Ordering::AcqRel);
    while UNTIL.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    take_turns(cpu::this_cpu().get() as u64, MIN_HANDOFFS);
    DONE.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
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
    let mut aps = 0u64;
    for &id in cpus.iter().filter(|&&id| id != bsp) {
        if crate::sched::spawn(id, turn_taker, 0).is_err() {
            let _ = writeln!(
                com1,
                "origin-kernel: clock: cannot spawn on CPU {id}: clock failed"
            );
            return false;
        }
        aps += 1;
    }
    crate::sched::yield_until(|| READY.load(Ordering::Acquire) == aps, tsc_hz, 2);
    if READY.load(Ordering::Acquire) != aps {
        let _ = writeln!(
            com1,
            "origin-kernel: clock: turn-takers did not start: clock failed"
        );
        return false;
    }
    let start = rdtsc();
    LATEST.store(start.saturating_add(tsc_hz / 2), Ordering::Relaxed);
    UNTIL.store(start.saturating_add(tsc_hz / 10), Ordering::Release);
    take_turns(
        bsp.get() as u64,
        if cpus.len() < 2 { 0 } else { MIN_HANDOFFS },
    );
    crate::sched::yield_until(|| DONE.load(Ordering::Acquire) == aps, tsc_hz, 2);
    let done = DONE.load(Ordering::Acquire) == aps;
    let readings = READINGS.load(Ordering::Relaxed);
    let handoffs = HANDOFFS.load(Ordering::Relaxed);
    let backwards = BACKWARDS.load(Ordering::Relaxed);
    let interleaved = cpus.len() < 2 || handoffs >= MIN_HANDOFFS;
    let ok = done && backwards == 0 && interleaved;
    let _ = writeln!(
        com1,
        "origin-kernel: clock: TSC consistent across {} processors: {readings} ordered readings, {handoffs} handoffs between processors, {backwards} backwards (worst {} ticks){}",
        cpus.len(),
        WORST.load(Ordering::Relaxed),
        if ok { "" } else { ": clock failed" }
    );
    ok
}
