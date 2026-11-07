//! The kernel's clocks (CLAUDE.md §68): a monotonic clock in nanoseconds since kernel
//! entry, read from the TSC on any processor, and a UTC wall clock set at boot from the
//! CMOS RTC and advanced by the monotonic clock.
//!
//! At boot, on the boot processor, [`init`]:
//! 1. reads the ACPI FADT: whether the platform has a CMOS RTC, and where its century
//!    register is;
//! 2. times one RTC second on the TSC, between the ends of two consecutive update cycles
//!    (register A's update-in-progress bit falling), an oscillator independent of the PIT
//!    that calibrated the TSC;
//! 3. chooses the TSC rate: CPUID leaf 15h's if enumerated and within 1 % of the PIT
//!    calibration; else the RTC second's, if within 1 %; else the PIT calibration;
//! 4. reads the RTC just after the first edge, so the wall clock is right to within the
//!    polling latency (and, on real hardware, the 1984 µs update cycle), not to a second.
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
use framekernel::clock::{self, NANOS_PER_SECOND, Scale, SharedScale};
use framekernel::hpet;
use framekernel::rtc::{self, DateTime, Registers, register};
use origin_sync::SpinLock;

use crate::console::Console;
use crate::cpu::{self, rdtsc};
use crate::{inb, outb};

/// The TSC when the kernel was entered: the monotonic clock's zero.
static ENTRY_TSC: AtomicU64 = AtomicU64::new(0);
/// TSC ticks to nanoseconds, once [`init`] has chosen the rate.
static SCALE: SharedScale = SharedScale::new();
/// Unix time in nanoseconds at monotonic zero; 0 while unknown.
static BOOT_WALL: AtomicU64 = AtomicU64::new(0);

/// Records the monotonic clock's zero: the first thing `_start` does.
pub(crate) fn mark_entry() {
    ENTRY_TSC.store(rdtsc(), Ordering::Relaxed);
}

/// Nanoseconds since kernel entry; 0 until [`init`] has chosen the TSC rate.
pub(crate) fn monotonic_nanos() -> u64 {
    let Some(scale) = SCALE.get() else {
        return 0;
    };
    scale.nanos(rdtsc().saturating_sub(ENTRY_TSC.load(Ordering::Relaxed)))
}

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

/// Waits for the end of the RTC's next update cycle (the update-in-progress bit falling)
/// and returns the TSC there, or `None` if none comes within `limit` TSC ticks. The time
/// registers then hold the new second and stay stable for almost a second.
fn update_end(cmos: &mut Cmos, limit: u64) -> Option<u64> {
    let start = rdtsc();
    let expired = || rdtsc().wrapping_sub(start) > limit;
    while !cmos.updating() {
        if expired() {
            return None;
        }
    }
    while cmos.updating() {
        if expired() {
            return None;
        }
    }
    Some(rdtsc())
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
    // One RTC second on the TSC, and the RTC reading at its start.
    let mut edge = None;
    let mut second = None;
    if rtc_present {
        // SAFETY: CPL 0, the boot processor, interrupts disabled; no other code uses the
        // CMOS ports.
        let mut cmos = unsafe { Cmos::new() };
        let limit = calibrated_hz.saturating_add(calibrated_hz / 5); // 1.2 s
        if let Some(t1) = update_end(&mut cmos, limit) {
            let read = cmos.stable_registers(century);
            if let Some(t2) = update_end(&mut cmos, limit) {
                second = Some(t2.wrapping_sub(t1));
            }
            edge = Some((t1, read));
        }
    }
    let cpuid_hz = cpuid_tsc_hz();
    let (hz, source) = match (cpuid_hz, second) {
        (Some(c), _) if clock::rates_agree(c, calibrated_hz, 10) => (c, "CPUID 15h"),
        (_, Some(r)) if clock::rates_agree(r, calibrated_hz, 10) => (r, "one RTC second"),
        _ => (calibrated_hz, "the PIT calibration"),
    };
    let Some(scale) = Scale::new(hz) else {
        let _ = writeln!(com1, "origin-kernel: clock: TSC rate 0: clock failed");
        return false;
    };
    SCALE.publish(scale);
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
        Some(ticks) => {
            let micros = u128::from(ticks) * 1_000_000 / u128::from(calibrated_hz.max(1));
            let right = (990_000..=1_010_000).contains(&micros);
            ok &= right;
            let _ = writeln!(
                com1,
                "origin-kernel: clock: one RTC second lasted {micros} us on the PIT-calibrated TSC{}",
                if right { "" } else { ": clock failed" }
            );
        }
        None if rtc_present => {
            ok = false;
            let _ = writeln!(
                com1,
                "origin-kernel: clock: the RTC did not complete two updates in 1.2 s: clock failed"
            );
        }
        None => {}
    }
    ok &= set_wall_clock(com1, scale, edge, century, rtc_present);
    ok &= prove_hpet(com1, rsdp, scale);
    ok
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
    // Measure over about 20 ms of the TSC.
    let c0 = hpet_read(virt, hpet::register::COUNTER);
    let t0 = rdtsc();
    let window = scale.hz() / 50;
    while rdtsc().wrapping_sub(t0) < window {
        core::hint::spin_loop();
    }
    let c1 = hpet_read(virt, hpet::register::COUNTER);
    let t1 = rdtsc();
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

/// Sets the wall clock from the RTC reading taken at TSC `t1`, just after an update.
fn set_wall_clock(
    com1: &mut Console,
    scale: Scale,
    edge: Option<(u64, Option<Registers>)>,
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
    let Some((t1, Some(regs))) = edge else {
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
    let at = scale.nanos(t1.saturating_sub(ENTRY_TSC.load(Ordering::Relaxed)));
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
/// the reading before it, whichever processor took that one.
fn take_turns(me: u64) {
    let rdtscp = rdtscp_supported();
    let until = UNTIL.load(Ordering::Acquire);
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
            now >= until
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
    take_turns(cpu::cpu_id());
    DONE.fetch_add(1, Ordering::AcqRel);
    crate::sched::exit()
}

/// Proves that the TSC readings of every online processor are consistent: for 100 ms all
/// of them take ordered readings in turn, and no reading may be below the one taken just
/// before it on another processor. Runs from the boot flow with every scheduler active.
pub(crate) fn prove_consistent(com1: &mut Console, cpus: &[u32], bsp: usize, tsc_hz: u64) -> bool {
    let mut aps = 0u64;
    for &id in cpus.iter().filter(|&&id| id as usize != bsp) {
        if crate::sched::spawn(id as usize, turn_taker, 0).is_err() {
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
    UNTIL.store(rdtsc().saturating_add(tsc_hz / 10), Ordering::Release);
    take_turns(bsp as u64);
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
